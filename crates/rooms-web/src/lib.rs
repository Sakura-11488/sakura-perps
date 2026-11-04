//! Browser-facing PAPER scenario. No keys, custody, live oracle or durable store.
//!
//! The caller signs the returned canonical bytes. It must persist all candidate
//! records and their checkpoint atomically BEFORE publishing success, discard
//! this candidate on storage failure, and replay committed state after restart.
//! Browser storage cannot prove freshness against rollback by its own operator.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use sakura_perps_risk::{math::mul_div_floor, position::trade_fee};
use sakura_perps_rooms::*;
use sakura_perps_rooms_auth::{
    wire::{self, Action, Domain, Genesis, Intent, PrincipalBinding, SignedEnvelope},
    AuthOutcome, AuthenticatedRecord, AuthenticatedRoom, Evidence, ExecutionContext,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

pub const MAX_RECORDS: usize = 8;
pub const MAX_ACTION_JSON_BYTES: usize = 1_024;
pub const MAX_RECORDS_JSON_BYTES: usize = MAX_RECORDS * (2 * wire::MAX_WIRE_BYTES + 3) + 2;
pub const QUOTE_ATOMS: u128 = 1_000_000;
const ACTOR: Actor = Actor::Owner(OwnerId(1));
const ROOM: RoomId = RoomId(1);
const MEMBER: MemberId = MemberId(1);
const CELL: CellId = CellId(1);
const MARKET: MarketId = MarketId(1);
const POSITION: PositionId = PositionId(1);
const FEE_BPS: u16 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidInput(&'static str),
    LimitExceeded,
    WrongPhase,
    InvalidPilotAction,
    InvalidGenesis,
    TranscriptMismatch,
    Arithmetic,
    Wire(wire::WireError),
    Auth(sakura_perps_rooms_auth::Error),
    Kernel(sakura_perps_rooms::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<wire::WireError> for Error {
    fn from(value: wire::WireError) -> Self {
        Self::Wire(value)
    }
}
impl From<sakura_perps_rooms_auth::Error> for Error {
    fn from(value: sakura_perps_rooms_auth::Error) -> Self {
        Self::Auth(value)
    }
}
impl From<sakura_perps_rooms::Error> for Error {
    fn from(value: sakura_perps_rooms::Error) -> Self {
        Self::Kernel(value)
    }
}
impl From<sakura_perps_risk::RiskError> for Error {
    fn from(_: sakura_perps_risk::RiskError) -> Self {
        Self::Arithmetic
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Long,
    Short,
}
impl Direction {
    fn side(self) -> Side {
        match self {
            Self::Long => Side::Long,
            Self::Short => Side::Short,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Initialize {},
    Allocate {
        budget_credits: String,
        leverage: String,
        side: Direction,
    },
    RunScenario {},
}

#[derive(Clone, Copy)]
struct Allocation {
    budget: u128,
    leverage: u128,
    side: Direction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Prepared {
    pub step: &'static str,
    pub intent_hex: String,
    pub signing_bytes_hex: String,
}

/// All amounts are integer atoms of a six-decimal PAPER quote unit. Prices are
/// whole paper credits per SOL. No USD peg, live quote or executable midpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AllocationPreview {
    pub budget_credits: String,
    pub leverage: String,
    pub side: &'static str,
    pub budget_quote_atoms: String,
    pub collateral_quote_atoms: String,
    pub fee_quote_atoms: String,
    pub committed_quote_atoms: String,
    pub unused_quote_atoms: String,
    pub size_base_atoms: String,
    pub notional_quote_atoms: String,
    pub profit_cap_quote_atoms: String,
    pub maximum_payout_quote_atoms: String,
    pub midpoint_payout_quote_atoms: String,
    pub terminal_payout_quote_atoms: String,
    pub terminal_balance_quote_atoms: String,
    pub terminal_net_pnl_quote_atoms: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SettlementSummary {
    pub payout_quote_atoms: String,
    pub net_pnl_quote_atoms: String,
    pub fee_quote_atoms: String,
    pub realized_loss_quote_atoms: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub phase: &'static str,
    pub market: &'static str,
    pub quote_decimals: &'static str,
    pub base_decimals: &'static str,
    pub current_slot: String,
    pub record_count: String,
    pub initialized_steps: String,
    pub entry_price_credits: &'static str,
    pub midpoint_price_credits: &'static str,
    pub terminal_price_credits: &'static str,
    pub balance_quote_atoms: String,
    pub contributed_quote_atoms: String,
    pub loss_used_quote_atoms: String,
    pub reserved_quote_atoms: String,
    pub allocation: Option<AllocationPreview>,
    pub settlement: Option<SettlementSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Checkpoint {
    pub genesis_hash: String,
    pub record_count: String,
    pub head_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub record_hex: String,
    pub summary: Summary,
    pub checkpoint: Checkpoint,
}

/// Host-testable engine. Its in-memory state is a candidate until the browser's
/// durable transaction commits; it is not a persistence or signing API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    room: AuthenticatedRoom,
}

impl Session {
    pub fn create(key_hex: &str, nonce_hex: &str, plan_hash_hex: &str) -> Result<Self, Error> {
        let key = fixed_hex::<32>(key_hex)?;
        let nonce = fixed_hex::<32>(nonce_hex)?;
        let plan_hash = fixed_hex::<32>(plan_hash_hex)?;
        if nonce == [0; 32] || plan_hash == [0; 32] {
            return Err(Error::InvalidGenesis);
        }
        let mut deployment = Sha256::new();
        deployment.update(b"SakuraRoomsPaper/Deployment/v1\0");
        deployment.update(nonce);
        deployment.update(plan_hash);
        Self::from_genesis(Genesis {
            domain: Domain {
                network: network(),
                deployment: deployment.finalize().into(),
                room: ROOM,
            },
            administrator: OwnerId(1),
            principals: vec![PrincipalBinding {
                actor: ACTOR,
                public_key: key,
            }],
        })
    }

    fn from_genesis(genesis: Genesis) -> Result<Self, Error> {
        if genesis.domain.network != network()
            || genesis.domain.room != ROOM
            || genesis.administrator != OwnerId(1)
            || genesis.principals.len() != 1
            || genesis.principals[0].actor != ACTOR
        {
            return Err(Error::InvalidGenesis);
        }
        Ok(Self {
            room: AuthenticatedRoom::new(genesis)?,
        })
    }

    /// Replay every record through BOTH the fixed scenario facade and signature
    /// gateway. Root metadata must independently recompute the expected genesis
    /// from public key, nonce and immutable plan hash before accepting this store.
    pub fn restore(
        genesis_hex: &str,
        records_json: &str,
        expected_head_hex: &str,
    ) -> Result<Self, Error> {
        if records_json.len() > MAX_RECORDS_JSON_BYTES {
            return Err(Error::LimitExceeded);
        }
        let records: Vec<String> =
            serde_json::from_str(records_json).map_err(|_| Error::InvalidInput("records_json"))?;
        if records.len() > MAX_RECORDS {
            return Err(Error::LimitExceeded);
        }
        let genesis: Genesis = wire::decode(&decode_hex(genesis_hex, wire::MAX_WIRE_BYTES)?)?;
        let expected_head = fixed_hex::<32>(expected_head_hex)?;
        let mut session = Self::from_genesis(genesis)?;
        for encoded in records {
            let record: AuthenticatedRecord =
                wire::decode(&decode_hex(&encoded, wire::MAX_WIRE_BYTES)?)?;
            let applied = match &record.evidence {
                Evidence::Signed { envelope, .. } => session.submit(
                    &hex(&wire::encode(&envelope.intent)?),
                    &hex(&envelope.signature),
                )?,
                Evidence::Matured { position, now_slot } => {
                    if *position != POSITION || *now_slot != 3 {
                        return Err(Error::InvalidPilotAction);
                    }
                    session.settle_scenario()?
                }
            };
            // This additionally checks operator context, hash, index and output.
            if applied.record_hex != encoded {
                return Err(Error::TranscriptMismatch);
            }
        }
        if session.room.head_hash() != expected_head {
            return Err(Error::TranscriptMismatch);
        }
        Ok(session)
    }

    pub fn genesis_hex(&self) -> Result<String, Error> {
        Ok(hex(&wire::encode(self.room.genesis())?))
    }
    pub fn records_json(&self) -> Result<String, Error> {
        let records = self
            .room
            .records()
            .iter()
            .map(|r| wire::encode(r).map(|b| hex(&b)))
            .collect::<Result<Vec<_>, _>>()?;
        json(&records)
    }
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            genesis_hash: hex(&self.room.genesis_hash()),
            record_count: self.room.records().len().to_string(),
            head_hash: hex(&self.room.head_hash()),
        }
    }

    pub fn prepare(&self, request_json: &str) -> Result<Prepared, Error> {
        let request = request(request_json)?;
        let count = self.room.records().len();
        let (step, action) = match request {
            Request::Initialize {} if count < 4 => setup(count)?,
            Request::Allocate {
                budget_credits,
                leverage,
                side,
            } if count == 4 || count == 5 => {
                let allocation = allocation(&budget_credits, &leverage, side)?;
                if count == 4 {
                    (
                        "contribute",
                        Action::Contribute {
                            member: MEMBER,
                            amount_quote: allocation.budget,
                        },
                    )
                } else {
                    if self.contributed()? != allocation.budget {
                        return Err(Error::InvalidInput("budget_mismatch"));
                    }
                    (
                        "open",
                        Action::Open {
                            intent: open_intent(allocation)?,
                        },
                    )
                }
            }
            Request::RunScenario {} if count == 6 => ("record_terminal", terminal()),
            _ => return Err(Error::WrongPhase),
        };
        let intent = self.intent(action)?;
        Ok(Prepared {
            step,
            intent_hex: hex(&wire::encode(&intent)?),
            signing_bytes_hex: hex(&wire::signing_bytes(&intent)?),
        })
    }

    pub fn submit(&mut self, intent_hex: &str, signature_hex: &str) -> Result<Applied, Error> {
        let intent: Intent = wire::decode(&decode_hex(intent_hex, wire::MAX_WIRE_BYTES)?)?;
        let signature = fixed_hex::<64>(signature_hex)?;
        let expected = self.expected_action(&intent.action)?;
        if intent != self.intent(expected)? {
            return Err(Error::InvalidPilotAction);
        }
        let context = context(&intent.action);
        let mut planned = self.clone();
        let record = planned.room.execute(
            &wire::encode(&SignedEnvelope { intent, signature })?,
            context,
        )?;
        let applied = planned.applied(&record)?;
        *self = planned;
        Ok(applied)
    }

    pub fn settle_scenario(&mut self) -> Result<Applied, Error> {
        if self.room.records().len() != 7 {
            return Err(Error::WrongPhase);
        }
        let mut planned = self.clone();
        let record = planned.room.settle_matured(POSITION, 3)?;
        let applied = planned.applied(&record)?;
        *self = planned;
        Ok(applied)
    }

    fn applied(&self, record: &AuthenticatedRecord) -> Result<Applied, Error> {
        Ok(Applied {
            record_hex: hex(&wire::encode(record)?),
            summary: self.summary()?,
            checkpoint: self.checkpoint(),
        })
    }

    fn intent(&self, action: Action) -> Result<Intent, Error> {
        let slot = context(&action).now_slot;
        Ok(Intent {
            genesis_hash: self.room.genesis_hash(),
            actor: ACTOR,
            sequence: self
                .room
                .next_sequence(ACTOR)
                .ok_or(Error::InvalidGenesis)?,
            not_before_slot: slot,
            expires_slot: slot,
            authorization_revision: matches!(action, Action::Open { .. }).then_some(1),
            action,
        })
    }

    fn contributed(&self) -> Result<u128, Error> {
        self.room
            .room()
            .member(MEMBER)
            .map(|m| m.contributed_quote)
            .ok_or(Error::WrongPhase)
    }

    fn expected_action(&self, supplied: &Action) -> Result<Action, Error> {
        match self.room.records().len() {
            count @ 0..=3 => Ok(setup(count)?.1),
            4 => {
                let Action::Contribute {
                    member,
                    amount_quote,
                } = supplied
                else {
                    return Err(Error::InvalidPilotAction);
                };
                if *member != MEMBER
                    || *amount_quote % QUOTE_ATOMS != 0
                    || !(QUOTE_ATOMS..=100 * QUOTE_ATOMS).contains(amount_quote)
                {
                    return Err(Error::InvalidPilotAction);
                }
                Ok(supplied.clone())
            }
            5 => {
                let Action::Open { intent } = supplied else {
                    return Err(Error::InvalidPilotAction);
                };
                let allocation = allocation_from_open(intent, self.contributed()?)?;
                Ok(Action::Open {
                    intent: open_intent(allocation)?,
                })
            }
            6 => Ok(terminal()),
            _ => Err(Error::WrongPhase),
        }
    }

    pub fn summary(&self) -> Result<Summary, Error> {
        let count = self.room.records().len();
        let member = self.room.room().member(MEMBER);
        let contributed = member.map_or(0, |m| m.contributed_quote);
        let open = self.room.records().iter().find_map(|r| match &r.outcome {
            AuthOutcome::Kernel(Outcome::Opened(p)) => Some(p.as_ref()),
            _ => None,
        });
        let closed = self.room.records().iter().find_map(|r| match &r.outcome {
            AuthOutcome::Kernel(Outcome::Closed(p)) => Some(p),
            _ => None,
        });
        let allocation = open
            .map(|p| preview_from_receipt(p, contributed))
            .transpose()?;
        let settlement = match (open, closed) {
            (Some(p), Some(c)) => Some(SettlementSummary {
                payout_quote_atoms: c.payout_quote.to_string(),
                net_pnl_quote_atoms: difference(member.map_or(0, |m| m.free_quote), contributed)?,
                fee_quote_atoms: p.fee_quote.to_string(),
                realized_loss_quote_atoms: c.realized_loss_quote.to_string(),
            }),
            _ => None,
        };
        Ok(Summary {
            phase: match count {
                0..=3 => "initializing",
                4 => "ready",
                5 => "funded",
                6 => "open",
                7 => "terminal_recorded",
                8 => "settled",
                _ => return Err(Error::LimitExceeded),
            },
            market: "SOL / PAPER",
            quote_decimals: "6",
            base_decimals: "6",
            current_slot: self.room.current_slot().to_string(),
            record_count: count.to_string(),
            initialized_steps: count.min(4).to_string(),
            entry_price_credits: "100",
            midpoint_price_credits: "92",
            terminal_price_credits: "104",
            balance_quote_atoms: member.map_or(0, |m| m.free_quote).to_string(),
            contributed_quote_atoms: contributed.to_string(),
            loss_used_quote_atoms: member.map_or(0, |m| m.loss_and_fee_used_quote).to_string(),
            reserved_quote_atoms: self
                .room
                .room()
                .cell(CELL)
                .map_or(0, |c| c.reserved_quote)
                .to_string(),
            allocation,
            settlement,
        })
    }
}

fn network() -> [u8; 32] {
    Sha256::digest(b"SakuraRoomsPaper/Network/v1\0").into()
}

fn setup(index: usize) -> Result<(&'static str, Action), Error> {
    Ok(match index {
        0 => (
            "register_market",
            Action::RegisterMarket {
                market: Market {
                    id: MARKET,
                    generation: 1,
                    asset_decimals: 6,
                    min_price: 10 * PRICE_SCALE,
                    max_price: 200 * PRICE_SCALE,
                    max_oracle_age_slots: 0,
                    max_position_lifetime_slots: 2,
                    fee_bps: FEE_BPS,
                },
            },
        ),
        1 => (
            "add_cell",
            Action::AddCell {
                cell: CELL,
                policy: UnderwritingPolicy {
                    allowed_members: BTreeSet::from([(MEMBER, OwnerId(1), 1)]),
                    allowed_market_generations: BTreeSet::from([(MARKET, 1)]),
                    max_position_notional_quote: 300 * QUOTE_ATOMS,
                    max_profit_cap_quote: 50 * QUOTE_ATOMS,
                    max_leverage_bps: 30_000,
                    max_total_reserved_quote: 150 * QUOTE_ATOMS,
                    min_fee_bps: FEE_BPS,
                },
            },
        ),
        2 => (
            "fund_cell",
            Action::FundCell {
                cell: CELL,
                amount_quote: 1_000 * QUOTE_ATOMS,
            },
        ),
        3 => (
            "add_member",
            Action::AddMember {
                member: MEMBER,
                delegate: None,
                policy: Policy {
                    contribution_limit_quote: 100 * QUOTE_ATOMS,
                    loss_limit_quote: 100 * QUOTE_ATOMS,
                    allowed_markets: BTreeSet::from([MARKET]),
                    allowed_cells: BTreeSet::from([CELL]),
                    max_leverage_bps: 30_000,
                    max_position_notional_quote: 300 * QUOTE_ATOMS,
                    max_total_notional_quote: 300 * QUOTE_ATOMS,
                    max_open_positions: 1,
                    max_fee_bps: FEE_BPS,
                    max_intent_lifetime_slots: 1,
                },
            },
        ),
        _ => return Err(Error::WrongPhase),
    })
}

fn terminal() -> Action {
    Action::RecordTerminalObservation {
        position: POSITION,
        observation: quote(104, 3),
    }
}
fn quote(price: u128, slot: u64) -> Observation {
    Observation {
        market: MARKET,
        generation: 1,
        price: price * PRICE_SCALE,
        published_slot: slot,
    }
}
fn context(action: &Action) -> ExecutionContext {
    ExecutionContext {
        now_slot: if matches!(action, Action::RecordTerminalObservation { .. }) {
            3
        } else {
            1
        },
        observation: matches!(action, Action::Open { .. }).then(|| quote(100, 1)),
    }
}

fn allocation(budget: &str, leverage: &str, side: Direction) -> Result<Allocation, Error> {
    let budget = integer(budget)?;
    let leverage = integer(leverage)?;
    if !(1..=100).contains(&budget) {
        return Err(Error::InvalidInput("budget_credits"));
    }
    if !(1..=3).contains(&leverage) {
        return Err(Error::InvalidInput("leverage"));
    }
    Ok(Allocation {
        budget: budget.checked_mul(QUOTE_ATOMS).ok_or(Error::Arithmetic)?,
        leverage,
        side,
    })
}

fn open_intent(allocation: Allocation) -> Result<OpenIntent, Error> {
    // Round collateral down to 100 quote atoms, so the exact 100-credit SOL
    // entry has integral base atoms and EXACT chosen leverage. Budget includes
    // the rounded-up entry fee; the tiny remainder remains freely allocated.
    let fee_factor = allocation
        .leverage
        .checked_mul(u128::from(FEE_BPS))
        .and_then(|v| v.checked_add(10_000))
        .ok_or(Error::Arithmetic)?;
    let collateral = mul_div_floor(allocation.budget, 10_000, fee_factor)? / 100 * 100;
    let notional = collateral
        .checked_mul(allocation.leverage)
        .ok_or(Error::Arithmetic)?;
    let fee = trade_fee(notional, FEE_BPS)?;
    if collateral == 0 || collateral.checked_add(fee).ok_or(Error::Arithmetic)? > allocation.budget
    {
        return Err(Error::Arithmetic);
    }
    Ok(OpenIntent {
        consent: Consent {
            room: ROOM,
            member: MEMBER,
            member_generation: 1,
            policy_revision: 1,
            market_generation: 1,
            nonce: 0,
            issued_slot: 1,
            expires_slot: 1,
            min_execution_price: 100 * PRICE_SCALE,
            max_execution_price: 100 * PRICE_SCALE,
        },
        market: MARKET,
        cell: CELL,
        cell_policy_revision: 1,
        side: allocation.side.side(),
        size_base: notional / 100,
        collateral_quote: collateral,
        profit_cap_quote: collateral / 2,
        max_fee_quote: fee,
        maturity_slot: 3,
    })
}

fn allocation_from_open(intent: &OpenIntent, budget: u128) -> Result<Allocation, Error> {
    let notional = intent.size_base.checked_mul(100).ok_or(Error::Arithmetic)?;
    if intent.collateral_quote == 0 || notional % intent.collateral_quote != 0 {
        return Err(Error::InvalidPilotAction);
    }
    let leverage = notional / intent.collateral_quote;
    if !(1..=3).contains(&leverage) {
        return Err(Error::InvalidPilotAction);
    }
    Ok(Allocation {
        budget,
        leverage,
        side: match intent.side {
            Side::Long => Direction::Long,
            Side::Short => Direction::Short,
        },
    })
}

fn preview_from_receipt(receipt: &OpenReceipt, budget: u128) -> Result<AllocationPreview, Error> {
    let allocation = allocation_from_open(&receipt.intent, budget)?;
    let committed = receipt
        .intent
        .collateral_quote
        .checked_add(receipt.fee_quote)
        .ok_or(Error::Arithmetic)?;
    let unused = budget.checked_sub(committed).ok_or(Error::Arithmetic)?;
    let terminal = receipt.payout_at(104 * PRICE_SCALE)?;
    let final_balance = unused.checked_add(terminal).ok_or(Error::Arithmetic)?;
    Ok(AllocationPreview {
        budget_credits: (budget / QUOTE_ATOMS).to_string(),
        leverage: allocation.leverage.to_string(),
        side: match allocation.side {
            Direction::Long => "long",
            Direction::Short => "short",
        },
        budget_quote_atoms: budget.to_string(),
        collateral_quote_atoms: receipt.intent.collateral_quote.to_string(),
        fee_quote_atoms: receipt.fee_quote.to_string(),
        committed_quote_atoms: committed.to_string(),
        unused_quote_atoms: unused.to_string(),
        size_base_atoms: receipt.intent.size_base.to_string(),
        notional_quote_atoms: receipt.entry_notional_quote.to_string(),
        profit_cap_quote_atoms: receipt.intent.profit_cap_quote.to_string(),
        maximum_payout_quote_atoms: receipt.maximum_payout_quote.to_string(),
        midpoint_payout_quote_atoms: receipt.payout_at(92 * PRICE_SCALE)?.to_string(),
        terminal_payout_quote_atoms: terminal.to_string(),
        terminal_balance_quote_atoms: final_balance.to_string(),
        terminal_net_pnl_quote_atoms: difference(final_balance, budget)?,
    })
}

fn preview(request_json: &str) -> Result<AllocationPreview, Error> {
    let Request::Allocate {
        budget_credits,
        leverage,
        side,
    } = request(request_json)?
    else {
        return Err(Error::InvalidInput("allocate_action_required"));
    };
    let allocation = allocation(&budget_credits, &leverage, side)?;
    // Execute the SAME economic kernel used after signature verification. This
    // is an unsigned quote preview only; it cannot alter an authenticated room.
    let mut room = Room::new(ROOM, OwnerId(1));
    for i in 0..4 {
        room.execute(preview_command(setup(i)?.1)?)?;
    }
    room.execute(Command::Contribute {
        actor: ACTOR,
        member: MEMBER,
        amount_quote: allocation.budget,
    })?;
    let Outcome::Opened(receipt) = room.execute(Command::Open {
        actor: ACTOR,
        intent: open_intent(allocation)?,
        observation: quote(100, 1),
        now_slot: 1,
    })?
    else {
        return Err(Error::InvalidPilotAction);
    };
    preview_from_receipt(&receipt, allocation.budget)
}

fn preview_command(action: Action) -> Result<Command, Error> {
    Ok(match action {
        Action::RegisterMarket { market } => Command::RegisterMarket {
            actor: ACTOR,
            market,
        },
        Action::AddCell { cell, policy } => Command::AddCell {
            actor: ACTOR,
            cell,
            policy,
        },
        Action::FundCell { cell, amount_quote } => Command::FundCell {
            actor: ACTOR,
            cell,
            amount_quote,
        },
        Action::AddMember {
            member,
            delegate,
            policy,
        } => Command::AddMember {
            actor: ACTOR,
            member,
            delegate,
            policy,
        },
        _ => return Err(Error::InvalidPilotAction),
    })
}

fn request(value: &str) -> Result<Request, Error> {
    if value.len() > MAX_ACTION_JSON_BYTES {
        return Err(Error::LimitExceeded);
    }
    serde_json::from_str(value).map_err(|_| Error::InvalidInput("action_json"))
}
fn integer(value: &str) -> Result<u128, Error> {
    if value.is_empty()
        || value.len() > 3
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(Error::InvalidInput("canonical_integer"));
    }
    value
        .parse()
        .map_err(|_| Error::InvalidInput("canonical_integer"))
}
fn difference(a: u128, b: u128) -> Result<String, Error> {
    Ok(i128::try_from(a)
        .map_err(|_| Error::Arithmetic)?
        .checked_sub(i128::try_from(b).map_err(|_| Error::Arithmetic)?)
        .ok_or(Error::Arithmetic)?
        .to_string())
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}
fn decode_hex(value: &str, maximum: usize) -> Result<Vec<u8>, Error> {
    if value.len() > maximum * 2 {
        return Err(Error::LimitExceeded);
    }
    if value.len() % 2 != 0 {
        return Err(Error::InvalidInput("hex"));
    }
    let nibble = |b| match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(Error::InvalidInput("hex")),
    };
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| Ok(nibble(p[0])? * 16 + nibble(p[1])?))
        .collect()
}
fn fixed_hex<const N: usize>(value: &str) -> Result<[u8; N], Error> {
    decode_hex(value, N)?
        .try_into()
        .map_err(|_| Error::InvalidInput("hex_length"))
}
fn json<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::InvalidInput("json_encoding"))
}
fn js_error(error: Error) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[wasm_bindgen]
pub struct WebRoom {
    inner: Session,
}

#[wasm_bindgen]
impl WebRoom {
    pub fn create(key_hex: &str, nonce_hex: &str, plan_hash_hex: &str) -> Result<WebRoom, JsValue> {
        Session::create(key_hex, nonce_hex, plan_hash_hex)
            .map(|inner| Self { inner })
            .map_err(js_error)
    }
    pub fn restore(
        genesis_hex: &str,
        records_json: &str,
        expected_head_hex: &str,
    ) -> Result<WebRoom, JsValue> {
        Session::restore(genesis_hex, records_json, expected_head_hex)
            .map(|inner| Self { inner })
            .map_err(js_error)
    }
    pub fn genesis_hex(&self) -> Result<String, JsValue> {
        self.inner.genesis_hex().map_err(js_error)
    }
    pub fn records_json(&self) -> Result<String, JsValue> {
        self.inner.records_json().map_err(js_error)
    }
    pub fn prepare(&self, action_json: &str) -> Result<String, JsValue> {
        self.inner
            .prepare(action_json)
            .and_then(|r| json(&r))
            .map_err(js_error)
    }
    pub fn submit(&mut self, intent_hex: &str, signature_hex: &str) -> Result<String, JsValue> {
        self.inner
            .submit(intent_hex, signature_hex)
            .and_then(|r| json(&r))
            .map_err(js_error)
    }
    pub fn settle_scenario(&mut self) -> Result<String, JsValue> {
        self.inner
            .settle_scenario()
            .and_then(|r| json(&r))
            .map_err(js_error)
    }
    pub fn summary_json(&self) -> Result<String, JsValue> {
        self.inner
            .summary()
            .and_then(|r| json(&r))
            .map_err(js_error)
    }
    pub fn checkpoint_json(&self) -> Result<String, JsValue> {
        json(&self.inner.checkpoint()).map_err(js_error)
    }
}

#[wasm_bindgen]
pub fn preview_allocation(action_json: &str) -> Result<String, JsValue> {
    preview(action_json)
        .and_then(|r| json(&r))
        .map_err(js_error)
}

#[cfg(test)]
mod tests;
