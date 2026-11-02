//! Experimental PAPER simulator. No keys, signatures, tokens, RPC or on-chain program.
//!
//! A room coordinates independently owned member budgets. Every capped position
//! has its maximum payout reserved inside one backing cell before it opens.
//! Owners and delegates here are asserted simulation identities, not authenticated
//! principals. See the crate README for economics and limits.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use sakura_perps_risk::{
    math::{cmp_products, mul_div_ceil},
    position::{equity, notional_usd_ceil, settle_close, unrealized_pnl},
    RiskError,
};
pub use sakura_perps_risk::{scale::PRICE_SCALE, Side};

macro_rules! id {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(pub u64);
    )+ };
}
id!(RoomId, OwnerId, DelegateId, MemberId, CellId, MarketId, PositionId);

/// Caller-asserted role; the simulator does not verify any cryptographic proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    Owner(OwnerId),
    Delegate(DelegateId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Unauthorized,
    UnknownMember,
    UnknownCell,
    UnknownMarket,
    UnknownPosition,
    AlreadyExists,
    InvalidPolicy,
    InvalidAmount,
    WrongRoom,
    WrongGeneration,
    WrongPolicy,
    WrongCellPolicy,
    UnderwritingDenied,
    WrongNonce,
    ExpiredIntent,
    InvalidTime,
    StaleOracle,
    FutureOracle,
    MissingMaturityObservation,
    PriceOutOfBounds,
    MarketNotAllowed,
    CellNotAllowed,
    ContributionBudget,
    LossBudget,
    InsufficientBalance,
    InsufficientBacking,
    NotionalLimit,
    LeverageLimit,
    PositionLimit,
    FeeLimit,
    Arithmetic,
    Invariant,
    ReplayMismatch,
}

impl From<RiskError> for Error {
    fn from(_: RiskError) -> Self {
        Self::Arithmetic
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

fn add(a: u128, b: u128) -> Result<u128, Error> {
    a.checked_add(b).ok_or(Error::Arithmetic)
}
fn sub(a: u128, b: u128) -> Result<u128, Error> {
    a.checked_sub(b).ok_or(Error::Arithmetic)
}
fn next(a: u64) -> Result<u64, Error> {
    a.checked_add(1).ok_or(Error::Arithmetic)
}

/// Future-entry policy. All quote quantities are atoms of an abstract 6-decimal
/// quote unit; this model makes no dollar peg or real-world loss guarantee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub contribution_limit_quote: u128,
    /// Lifetime gross losses + entry fees + collateral currently at risk.
    /// Gains never replenish this budget. Effective cap also cannot exceed
    /// the member's actual cumulative external contributions.
    pub loss_limit_quote: u128,
    pub allowed_markets: BTreeSet<MarketId>,
    pub allowed_cells: BTreeSet<CellId>,
    pub max_leverage_bps: u32,
    pub max_position_notional_quote: u128,
    pub max_total_notional_quote: u128,
    pub max_open_positions: u16,
    pub max_fee_bps: u16,
    pub max_intent_lifetime_slots: u64,
}

impl Policy {
    fn validate(&self) -> Result<(), Error> {
        if self.contribution_limit_quote == 0
            || self.loss_limit_quote == 0
            || self.loss_limit_quote > self.contribution_limit_quote
            || self.allowed_markets.is_empty()
            || self.allowed_cells.is_empty()
            || self.max_leverage_bps == 0
            || self.max_position_notional_quote == 0
            || self.max_total_notional_quote < self.max_position_notional_quote
            || self.max_open_positions == 0
            || self.max_fee_bps > 10_000
            || self.max_intent_lifetime_slots == 0
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(())
    }
}

/// Backing-owner opt-in for future entries. Empty admission sets pause entries.
/// Existing receipts retain their terms and reserves through policy changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnderwritingPolicy {
    /// Bind admission to identity and incarnation, including before registration.
    pub allowed_members: BTreeSet<(MemberId, OwnerId, u64)>,
    pub allowed_market_generations: BTreeSet<(MarketId, u64)>,
    pub max_position_notional_quote: u128,
    pub max_profit_cap_quote: u128,
    pub max_leverage_bps: u32,
    pub max_total_reserved_quote: u128,
    pub min_fee_bps: u16,
}

impl UnderwritingPolicy {
    fn validate(&self) -> Result<(), Error> {
        if self.max_position_notional_quote == 0
            || self.max_leverage_bps == 0
            || self.max_total_reserved_quote == 0
            || self.min_fee_bps > 10_000
            || self
                .allowed_members
                .iter()
                .any(|(_, _, generation)| *generation == 0)
            || self
                .allowed_market_generations
                .iter()
                .any(|(_, generation)| *generation == 0)
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(())
    }
}

/// A generation's immutable oracle and fee assumptions. The administrator may
/// register the next generation, but an old position retains this full snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    pub id: MarketId,
    pub generation: u64,
    pub asset_decimals: u8,
    pub min_price: u128,
    pub max_price: u128,
    pub max_oracle_age_slots: u64,
    pub max_position_lifetime_slots: u64,
    pub fee_bps: u16,
}

impl Market {
    fn validate(&self) -> Result<(), Error> {
        if self.generation == 0
            || self.asset_decimals > 18
            || self.min_price == 0
            || self.max_price < self.min_price
            || self.max_position_lifetime_slots == 0
            || self.fee_bps > 10_000
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(())
    }

    fn check_price(&self, quote: &Observation, now: u64) -> Result<(), Error> {
        if quote.market != self.id || quote.generation != self.generation {
            return Err(Error::WrongGeneration);
        }
        if quote.published_slot > now {
            return Err(Error::FutureOracle);
        }
        if now - quote.published_slot > self.max_oracle_age_slots {
            return Err(Error::StaleOracle);
        }
        if quote.price < self.min_price || quote.price > self.max_price {
            return Err(Error::PriceOutOfBounds);
        }
        Ok(())
    }
}

/// Injected observation; ownership, publication and truth are assumed by the
/// paper scenario. A real implementation needs an authenticated oracle adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub market: MarketId,
    pub generation: u64,
    pub price: u128,
    pub published_slot: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    pub room: RoomId,
    pub member: MemberId,
    pub member_generation: u64,
    pub policy_revision: u64,
    pub market_generation: u64,
    pub nonce: u64,
    pub issued_slot: u64,
    pub expires_slot: u64,
    pub min_execution_price: u128,
    pub max_execution_price: u128,
}

impl Consent {
    fn check_time_and_limit(&self, policy: &Policy, now: u64, price: u128) -> Result<(), Error> {
        if self.issued_slot > now || self.expires_slot < self.issued_slot {
            return Err(Error::InvalidTime);
        }
        if now > self.expires_slot {
            return Err(Error::ExpiredIntent);
        }
        if self.expires_slot - self.issued_slot > policy.max_intent_lifetime_slots {
            return Err(Error::InvalidTime);
        }
        if self.min_execution_price == 0
            || self.max_execution_price < self.min_execution_price
            || price < self.min_execution_price
            || price > self.max_execution_price
        {
            return Err(Error::PriceOutOfBounds);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenIntent {
    pub consent: Consent,
    pub market: MarketId,
    pub cell: CellId,
    pub cell_policy_revision: u64,
    pub side: Side,
    pub size_base: u128,
    pub collateral_quote: u128,
    pub profit_cap_quote: u128,
    pub max_fee_quote: u128,
    /// Contract maturity, distinct from the authorization's expiry.
    pub maturity_slot: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseIntent {
    pub consent: Consent,
    pub position: PositionId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub owner: OwnerId,
    pub delegate: Option<DelegateId>,
    pub generation: u64,
    pub policy_revision: u64,
    pub policy: Policy,
    pub next_nonce: u64,
    pub free_quote: u128,
    pub contributed_quote: u128,
    pub withdrawn_quote: u128,
    pub loss_and_fee_used_quote: u128,
    pub collateral_at_risk_quote: u128,
    pub open_notional_quote: u128,
    pub open_positions: u16,
}

impl Member {
    fn authorize_trade(&self, actor: Actor) -> Result<(), Error> {
        if actor == Actor::Owner(self.owner)
            || matches!(actor, Actor::Delegate(id) if self.delegate == Some(id))
        {
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub owner: OwnerId,
    pub policy_revision: u64,
    pub policy: UnderwritingPolicy,
    /// Includes escrowed trader collateral and retained fees/losses.
    pub balance_quote: u128,
    /// Sum of complete maximum payouts, not only profit reserves.
    pub reserved_quote: u128,
    pub contributed_quote: u128,
    pub withdrawn_quote: u128,
}

impl Cell {
    pub fn free_quote(&self) -> Result<u128, Error> {
        sub(self.balance_quote, self.reserved_quote)
    }
}

/// The complete economic agreement actually executed. Copies returned to callers
/// cannot modify the archived receipt. Canonical fields replace an unverified hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReceipt {
    pub position: PositionId,
    pub owner: OwnerId,
    pub backing_owner: OwnerId,
    pub intent: OpenIntent,
    pub policy: Policy,
    pub cell_policy: UnderwritingPolicy,
    pub market: Market,
    pub observation: Observation,
    pub executed_slot: u64,
    pub entry_notional_quote: u128,
    pub fee_quote: u128,
    pub maximum_payout_quote: u128,
}

impl OpenReceipt {
    /// Recompute this capped contract's payout independently from current room
    /// policy. A zero-payout loss is final; there is no creditor claim on the owner.
    pub fn payout_at(&self, price: u128) -> Result<u128, Error> {
        if price < self.market.min_price || price > self.market.max_price {
            return Err(Error::PriceOutOfBounds);
        }
        let pnl_quote = unrealized_pnl(
            self.intent.side,
            self.intent.size_base,
            self.observation.price,
            price,
            self.market.asset_decimals,
        )?;
        let equity_quote = equity(self.intent.collateral_quote, pnl_quote, 0, 0)?;
        // Risk math's six-decimal USD quantity is used numerically as our
        // abstract six-decimal quote unit. No price feed or dollar peg is implied.
        let settlement = settle_close(
            self.intent.collateral_quote,
            self.intent.profit_cap_quote,
            equity_quote,
            0,
            6,
        )?;
        Ok(settlement.net_payout_quote)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseReceipt {
    pub position: PositionId,
    pub member: MemberId,
    pub owner: OwnerId,
    pub cell: CellId,
    pub policy_revision_at_open: u64,
    pub market_generation: u64,
    pub observation: Observation,
    pub executed_slot: u64,
    pub payout_quote: u128,
    pub realized_loss_quote: u128,
    pub released_reserve_quote: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    RegisterMarket {
        actor: Actor,
        market: Market,
    },
    AddCell {
        actor: Actor,
        cell: CellId,
        policy: UnderwritingPolicy,
    },
    ChangeCellPolicy {
        actor: Actor,
        cell: CellId,
        expected_revision: u64,
        policy: UnderwritingPolicy,
    },
    AddMember {
        actor: Actor,
        member: MemberId,
        delegate: Option<DelegateId>,
        policy: Policy,
    },
    ChangePolicy {
        actor: Actor,
        member: MemberId,
        expected_revision: u64,
        delegate: Option<DelegateId>,
        policy: Policy,
    },
    Contribute {
        actor: Actor,
        member: MemberId,
        amount_quote: u128,
    },
    FundCell {
        actor: Actor,
        cell: CellId,
        amount_quote: u128,
    },
    Withdraw {
        actor: Actor,
        member: MemberId,
        amount_quote: u128,
    },
    WithdrawCell {
        actor: Actor,
        cell: CellId,
        amount_quote: u128,
    },
    Open {
        actor: Actor,
        intent: OpenIntent,
        observation: Observation,
        now_slot: u64,
    },
    Close {
        actor: Actor,
        intent: CloseIntent,
        observation: Observation,
        now_slot: u64,
    },
    /// The asserted oracle administrator records one immutable terminal price
    /// shared by all contracts of this market generation and maturity slot.
    RecordTerminalObservation {
        actor: Actor,
        position: PositionId,
        observation: Observation,
        now_slot: u64,
    },
    /// Anyone may settle against the stored terminal observation. The member is
    /// the only beneficiary and no trade authorization nonce is consumed.
    SettleMatured {
        position: PositionId,
        now_slot: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    RegisteredMarket {
        market: MarketId,
        generation: u64,
    },
    AddedCell(CellId),
    ChangedCellPolicy {
        cell: CellId,
        revision: u64,
    },
    RecordedTerminalObservation(Observation),
    AddedMember(MemberId),
    ChangedPolicy {
        member: MemberId,
        revision: u64,
    },
    Contributed {
        member: MemberId,
        amount_quote: u128,
    },
    FundedCell {
        cell: CellId,
        amount_quote: u128,
    },
    /// Output is bound to the stored owner. No destination argument exists.
    Withdrawn {
        member: MemberId,
        owner: OwnerId,
        amount_quote: u128,
    },
    WithdrawnCell {
        cell: CellId,
        owner: OwnerId,
        amount_quote: u128,
    },
    Opened(Box<OpenReceipt>),
    Closed(CloseReceipt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub command: Command,
    pub outcome: Outcome,
}

/// In-memory state, private and changed only by atomic commands. Its ordered maps
/// and complete successful-command transcript make execution deterministic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    id: RoomId,
    administrator: OwnerId,
    members: BTreeMap<MemberId, Member>,
    cells: BTreeMap<CellId, Cell>,
    markets: BTreeMap<MarketId, Market>,
    positions: BTreeMap<PositionId, OpenReceipt>,
    terminal_observations: BTreeMap<(MarketId, u64, u64), Observation>,
    next_position: u64,
    current_slot: u64,
    external_in_quote: u128,
    external_out_quote: u128,
    records: Vec<Record>,
}

impl Room {
    pub fn new(id: RoomId, administrator: OwnerId) -> Self {
        Self {
            id,
            administrator,
            members: BTreeMap::new(),
            cells: BTreeMap::new(),
            markets: BTreeMap::new(),
            positions: BTreeMap::new(),
            terminal_observations: BTreeMap::new(),
            next_position: 1,
            current_slot: 0,
            external_in_quote: 0,
            external_out_quote: 0,
            records: Vec::new(),
        }
    }

    pub fn member(&self, id: MemberId) -> Option<&Member> {
        self.members.get(&id)
    }
    pub fn cell(&self, id: CellId) -> Option<&Cell> {
        self.cells.get(&id)
    }
    pub fn position(&self, id: PositionId) -> Option<&OpenReceipt> {
        self.positions.get(&id)
    }
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn external_flows(&self) -> (u128, u128) {
        (self.external_in_quote, self.external_out_quote)
    }
    pub fn current_slot(&self) -> u64 {
        self.current_slot
    }

    /// Commit only after all validation, arithmetic and reconciliation succeed.
    /// This also rolls back a nonce or any earlier mutation on a late error.
    pub fn execute(&mut self, command: Command) -> Result<Outcome, Error> {
        let mut planned = self.clone();
        if let Command::Open { now_slot, .. }
        | Command::Close { now_slot, .. }
        | Command::RecordTerminalObservation { now_slot, .. }
        | Command::SettleMatured { now_slot, .. } = &command
        {
            if *now_slot < planned.current_slot {
                return Err(Error::InvalidTime);
            }
            planned.current_slot = *now_slot;
        }
        let outcome = planned.apply(&command)?;
        planned.check_invariants()?;
        planned.records.push(Record {
            command,
            outcome: outcome.clone(),
        });
        *self = planned;
        Ok(outcome)
    }

    /// Replay all successful actions and verify the recorded economic outputs.
    /// This is reproducibility, not signature or transcript authenticity.
    pub fn replay(id: RoomId, administrator: OwnerId, records: &[Record]) -> Result<Self, Error> {
        let mut room = Self::new(id, administrator);
        for record in records {
            if room.execute(record.command.clone())? != record.outcome {
                return Err(Error::ReplayMismatch);
            }
        }
        Ok(room)
    }

    /// Deterministic inspection bytes for this crate version, including transcript.
    /// Rust Debug is NOT a stable wire codec or signing format. Used only to
    /// demonstrate unchanged in-memory state on failed simulated transactions.
    pub fn inspection_bytes(&self) -> Vec<u8> {
        format!("{self:?}").into_bytes()
    }

    fn require_owner(actor: Actor, owner: OwnerId) -> Result<(), Error> {
        if actor == Actor::Owner(owner) {
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    }

    fn consent_member(&self, actor: Actor, c: &Consent) -> Result<&Member, Error> {
        if c.room != self.id {
            return Err(Error::WrongRoom);
        }
        let member = self.members.get(&c.member).ok_or(Error::UnknownMember)?;
        member.authorize_trade(actor)?;
        if c.member_generation != member.generation {
            return Err(Error::WrongGeneration);
        }
        if c.nonce != member.next_nonce {
            return Err(Error::WrongNonce);
        }
        Ok(member)
    }

    fn apply(&mut self, command: &Command) -> Result<Outcome, Error> {
        match command {
            Command::RegisterMarket { actor, market } => {
                Self::require_owner(*actor, self.administrator)?;
                market.validate()?;
                let expected = match self.markets.get(&market.id) {
                    Some(old) => next(old.generation)?,
                    None => 1,
                };
                if market.generation != expected {
                    return Err(Error::WrongGeneration);
                }
                self.markets.insert(market.id, market.clone());
                Ok(Outcome::RegisteredMarket {
                    market: market.id,
                    generation: market.generation,
                })
            }
            Command::AddCell {
                actor,
                cell,
                policy,
            } => {
                let Actor::Owner(owner) = actor else {
                    return Err(Error::Unauthorized);
                };
                if self.cells.contains_key(cell) {
                    return Err(Error::AlreadyExists);
                }
                policy.validate()?;
                self.cells.insert(
                    *cell,
                    Cell {
                        owner: *owner,
                        policy_revision: 1,
                        policy: policy.clone(),
                        balance_quote: 0,
                        reserved_quote: 0,
                        contributed_quote: 0,
                        withdrawn_quote: 0,
                    },
                );
                Ok(Outcome::AddedCell(*cell))
            }
            Command::ChangeCellPolicy {
                actor,
                cell,
                expected_revision,
                policy,
            } => {
                let cell_state = self.cells.get_mut(cell).ok_or(Error::UnknownCell)?;
                Self::require_owner(*actor, cell_state.owner)?;
                policy.validate()?;
                if *expected_revision != cell_state.policy_revision {
                    return Err(Error::WrongCellPolicy);
                }
                cell_state.policy_revision = next(cell_state.policy_revision)?;
                cell_state.policy = policy.clone();
                Ok(Outcome::ChangedCellPolicy {
                    cell: *cell,
                    revision: cell_state.policy_revision,
                })
            }
            Command::AddMember {
                actor,
                member,
                delegate,
                policy,
            } => {
                let Actor::Owner(owner) = actor else {
                    return Err(Error::Unauthorized);
                };
                policy.validate()?;
                if self.members.contains_key(member) {
                    return Err(Error::AlreadyExists);
                }
                self.members.insert(
                    *member,
                    Member {
                        owner: *owner,
                        delegate: *delegate,
                        generation: 1,
                        policy_revision: 1,
                        policy: policy.clone(),
                        next_nonce: 0,
                        free_quote: 0,
                        contributed_quote: 0,
                        withdrawn_quote: 0,
                        loss_and_fee_used_quote: 0,
                        collateral_at_risk_quote: 0,
                        open_notional_quote: 0,
                        open_positions: 0,
                    },
                );
                Ok(Outcome::AddedMember(*member))
            }
            Command::ChangePolicy {
                actor,
                member,
                expected_revision,
                delegate,
                policy,
            } => {
                policy.validate()?;
                let m = self.members.get_mut(member).ok_or(Error::UnknownMember)?;
                Self::require_owner(*actor, m.owner)?;
                if *expected_revision != m.policy_revision {
                    return Err(Error::WrongPolicy);
                }
                if policy.contribution_limit_quote < m.contributed_quote {
                    return Err(Error::ContributionBudget);
                }
                // A stricter future-entry policy can coexist with grandfathered positions.
                // Existing receipts and lifetime counters are never rewritten.
                m.policy_revision = next(m.policy_revision)?;
                m.policy = policy.clone();
                m.delegate = *delegate;
                Ok(Outcome::ChangedPolicy {
                    member: *member,
                    revision: m.policy_revision,
                })
            }
            Command::Contribute {
                actor,
                member,
                amount_quote,
            } => {
                if *amount_quote == 0 {
                    return Err(Error::InvalidAmount);
                }
                let m = self.members.get_mut(member).ok_or(Error::UnknownMember)?;
                Self::require_owner(*actor, m.owner)?;
                let contributed = add(m.contributed_quote, *amount_quote)?;
                if contributed > m.policy.contribution_limit_quote {
                    return Err(Error::ContributionBudget);
                }
                m.contributed_quote = contributed;
                m.free_quote = add(m.free_quote, *amount_quote)?;
                self.external_in_quote = add(self.external_in_quote, *amount_quote)?;
                Ok(Outcome::Contributed {
                    member: *member,
                    amount_quote: *amount_quote,
                })
            }
            Command::FundCell {
                actor,
                cell,
                amount_quote,
            } => {
                if *amount_quote == 0 {
                    return Err(Error::InvalidAmount);
                }
                let c = self.cells.get_mut(cell).ok_or(Error::UnknownCell)?;
                Self::require_owner(*actor, c.owner)?;
                c.balance_quote = add(c.balance_quote, *amount_quote)?;
                c.contributed_quote = add(c.contributed_quote, *amount_quote)?;
                self.external_in_quote = add(self.external_in_quote, *amount_quote)?;
                Ok(Outcome::FundedCell {
                    cell: *cell,
                    amount_quote: *amount_quote,
                })
            }
            Command::Withdraw {
                actor,
                member,
                amount_quote,
            } => {
                if *amount_quote == 0 {
                    return Err(Error::InvalidAmount);
                }
                let m = self.members.get_mut(member).ok_or(Error::UnknownMember)?;
                Self::require_owner(*actor, m.owner)?;
                if *amount_quote > m.free_quote {
                    return Err(Error::InsufficientBalance);
                }
                m.free_quote = sub(m.free_quote, *amount_quote)?;
                m.withdrawn_quote = add(m.withdrawn_quote, *amount_quote)?;
                self.external_out_quote = add(self.external_out_quote, *amount_quote)?;
                Ok(Outcome::Withdrawn {
                    member: *member,
                    owner: m.owner,
                    amount_quote: *amount_quote,
                })
            }
            Command::WithdrawCell {
                actor,
                cell,
                amount_quote,
            } => {
                if *amount_quote == 0 {
                    return Err(Error::InvalidAmount);
                }
                let c = self.cells.get_mut(cell).ok_or(Error::UnknownCell)?;
                Self::require_owner(*actor, c.owner)?;
                if *amount_quote > c.free_quote()? {
                    return Err(Error::InsufficientBacking);
                }
                c.balance_quote = sub(c.balance_quote, *amount_quote)?;
                c.withdrawn_quote = add(c.withdrawn_quote, *amount_quote)?;
                self.external_out_quote = add(self.external_out_quote, *amount_quote)?;
                Ok(Outcome::WithdrawnCell {
                    cell: *cell,
                    owner: c.owner,
                    amount_quote: *amount_quote,
                })
            }
            Command::Open {
                actor,
                intent,
                observation,
                now_slot,
            } => self.open(*actor, intent, observation, *now_slot),
            Command::Close {
                actor,
                intent,
                observation,
                now_slot,
            } => self.close(*actor, intent, observation, *now_slot),
            Command::RecordTerminalObservation {
                actor,
                position,
                observation,
                now_slot,
            } => {
                Self::require_owner(*actor, self.administrator)?;
                let p = self.positions.get(position).ok_or(Error::UnknownPosition)?;
                if *now_slot < p.intent.maturity_slot {
                    return Err(Error::InvalidTime);
                }
                if observation.published_slot != p.intent.maturity_slot {
                    return Err(Error::MissingMaturityObservation);
                }
                p.market.check_price(observation, p.intent.maturity_slot)?;
                let key = (p.market.id, p.market.generation, p.intent.maturity_slot);
                if self.terminal_observations.contains_key(&key) {
                    return Err(Error::AlreadyExists);
                }
                self.terminal_observations.insert(key, observation.clone());
                Ok(Outcome::RecordedTerminalObservation(observation.clone()))
            }
            Command::SettleMatured { position, now_slot } => {
                self.settle_matured(*position, *now_slot)
            }
        }
    }

    fn open(
        &mut self,
        actor: Actor,
        intent: &OpenIntent,
        observation: &Observation,
        now: u64,
    ) -> Result<Outcome, Error> {
        let member = self.consent_member(actor, &intent.consent)?;
        if intent.consent.policy_revision != member.policy_revision {
            return Err(Error::WrongPolicy);
        }
        let policy = &member.policy;
        if !policy.allowed_markets.contains(&intent.market) {
            return Err(Error::MarketNotAllowed);
        }
        if !policy.allowed_cells.contains(&intent.cell) {
            return Err(Error::CellNotAllowed);
        }
        let market = self
            .markets
            .get(&intent.market)
            .ok_or(Error::UnknownMarket)?;
        if intent.consent.market_generation != market.generation {
            return Err(Error::WrongGeneration);
        }
        market.check_price(observation, now)?;
        intent
            .consent
            .check_time_and_limit(policy, now, observation.price)?;
        if intent.maturity_slot <= now
            || intent.maturity_slot - now > market.max_position_lifetime_slots
        {
            return Err(Error::InvalidTime);
        }
        if intent.size_base == 0 || intent.collateral_quote == 0 {
            return Err(Error::InvalidAmount);
        }
        let notional =
            notional_usd_ceil(intent.size_base, observation.price, market.asset_decimals)?;
        let total_notional = add(member.open_notional_quote, notional)?;
        if notional > policy.max_position_notional_quote
            || total_notional > policy.max_total_notional_quote
        {
            return Err(Error::NotionalLimit);
        }
        if cmp_products(
            notional,
            10_000,
            intent.collateral_quote,
            u128::from(policy.max_leverage_bps),
        )
        .is_gt()
        {
            return Err(Error::LeverageLimit);
        }
        if member.open_positions >= policy.max_open_positions {
            return Err(Error::PositionLimit);
        }
        let fee = mul_div_ceil(notional, u128::from(market.fee_bps), 10_000)?;
        if market.fee_bps > policy.max_fee_bps || fee > intent.max_fee_quote {
            return Err(Error::FeeLimit);
        }
        let debit = add(intent.collateral_quote, fee)?;
        if debit > member.free_quote {
            return Err(Error::InsufficientBalance);
        }
        let worst_loss = add(
            add(
                member.loss_and_fee_used_quote,
                member.collateral_at_risk_quote,
            )?,
            debit,
        )?;
        if worst_loss > policy.loss_limit_quote.min(member.contributed_quote) {
            return Err(Error::LossBudget);
        }
        let cell = self.cells.get(&intent.cell).ok_or(Error::UnknownCell)?;
        if intent.cell_policy_revision != cell.policy_revision {
            return Err(Error::WrongCellPolicy);
        }
        let underwriting = &cell.policy;
        let maximum_payout = add(intent.collateral_quote, intent.profit_cap_quote)?;
        if !underwriting.allowed_members.contains(&(
            intent.consent.member,
            member.owner,
            member.generation,
        )) || !underwriting
            .allowed_market_generations
            .contains(&(intent.market, market.generation))
            || notional > underwriting.max_position_notional_quote
            || intent.profit_cap_quote > underwriting.max_profit_cap_quote
            || add(cell.reserved_quote, maximum_payout)? > underwriting.max_total_reserved_quote
            || market.fee_bps < underwriting.min_fee_bps
            || cmp_products(
                notional,
                10_000,
                intent.collateral_quote,
                u128::from(underwriting.max_leverage_bps),
            )
            .is_gt()
        {
            return Err(Error::UnderwritingDenied);
        }
        // This profit reserve must come from this cell before the new entry fee.
        if intent.profit_cap_quote > cell.free_quote()? {
            return Err(Error::InsufficientBacking);
        }
        let receipt = OpenReceipt {
            position: PositionId(self.next_position),
            owner: member.owner,
            backing_owner: cell.owner,
            intent: intent.clone(),
            policy: policy.clone(),
            cell_policy: underwriting.clone(),
            market: market.clone(),
            observation: observation.clone(),
            executed_slot: now,
            entry_notional_quote: notional,
            fee_quote: fee,
            maximum_payout_quote: maximum_payout,
        };
        // Admission must not create a position whose legitimate in-band close
        // overflows. PnL/equity are monotone between the two extrema.
        receipt.payout_at(market.min_price)?;
        receipt.payout_at(market.max_price)?;
        self.next_position = next(self.next_position)?;
        let m = self
            .members
            .get_mut(&intent.consent.member)
            .ok_or(Error::UnknownMember)?;
        m.next_nonce = next(m.next_nonce)?;
        m.free_quote = sub(m.free_quote, debit)?;
        m.loss_and_fee_used_quote = add(m.loss_and_fee_used_quote, fee)?;
        m.collateral_at_risk_quote = add(m.collateral_at_risk_quote, intent.collateral_quote)?;
        m.open_notional_quote = total_notional;
        m.open_positions = m.open_positions.checked_add(1).ok_or(Error::Arithmetic)?;
        let c = self.cells.get_mut(&intent.cell).ok_or(Error::UnknownCell)?;
        c.balance_quote = add(c.balance_quote, debit)?;
        c.reserved_quote = add(c.reserved_quote, receipt.maximum_payout_quote)?;
        self.positions.insert(receipt.position, receipt.clone());
        Ok(Outcome::Opened(Box::new(receipt)))
    }

    fn close(
        &mut self,
        actor: Actor,
        intent: &CloseIntent,
        observation: &Observation,
        now: u64,
    ) -> Result<Outcome, Error> {
        self.consent_member(actor, &intent.consent)?;
        let p = self
            .positions
            .get(&intent.position)
            .ok_or(Error::UnknownPosition)?
            .clone();
        if intent.consent.member != p.intent.consent.member {
            return Err(Error::Unauthorized);
        }
        if intent.consent.market_generation != p.market.generation {
            return Err(Error::WrongGeneration);
        }
        if intent.consent.policy_revision != p.intent.consent.policy_revision {
            return Err(Error::WrongPolicy);
        }
        if now < p.executed_slot {
            return Err(Error::InvalidTime);
        }
        if now >= p.intent.maturity_slot {
            if self.terminal_observations.get(&(
                p.market.id,
                p.market.generation,
                p.intent.maturity_slot,
            )) != Some(observation)
            {
                return Err(Error::MissingMaturityObservation);
            }
            // A terminal observation is checked at maturity, not at replay time.
            // The injected historical observation still requires real authentication
            // outside this simulator; an absent terminal quote never gets guessed.
            p.market.check_price(observation, p.intent.maturity_slot)?;
        } else {
            p.market.check_price(observation, now)?;
        }
        intent
            .consent
            .check_time_and_limit(&p.policy, now, observation.price)?;
        self.settle_position(p, observation, now, true)
    }

    fn settle_matured(&mut self, position: PositionId, now: u64) -> Result<Outcome, Error> {
        let p = self
            .positions
            .get(&position)
            .ok_or(Error::UnknownPosition)?
            .clone();
        if now < p.intent.maturity_slot {
            return Err(Error::InvalidTime);
        }
        let observation = self
            .terminal_observations
            .get(&(p.market.id, p.market.generation, p.intent.maturity_slot))
            .ok_or(Error::MissingMaturityObservation)?
            .clone();
        self.settle_position(p, &observation, now, false)
    }

    fn settle_position(
        &mut self,
        p: OpenReceipt,
        observation: &Observation,
        now: u64,
        consume_nonce: bool,
    ) -> Result<Outcome, Error> {
        let payout = p.payout_at(observation.price)?;
        let loss = if payout < p.intent.collateral_quote {
            sub(p.intent.collateral_quote, payout)?
        } else {
            0
        };
        let m = self
            .members
            .get_mut(&p.intent.consent.member)
            .ok_or(Error::UnknownMember)?;
        if consume_nonce {
            m.next_nonce = next(m.next_nonce)?;
        }
        m.free_quote = add(m.free_quote, payout)?;
        m.loss_and_fee_used_quote = add(m.loss_and_fee_used_quote, loss)?;
        m.collateral_at_risk_quote = sub(m.collateral_at_risk_quote, p.intent.collateral_quote)?;
        m.open_notional_quote = sub(m.open_notional_quote, p.entry_notional_quote)?;
        m.open_positions = m.open_positions.checked_sub(1).ok_or(Error::Invariant)?;
        let c = self
            .cells
            .get_mut(&p.intent.cell)
            .ok_or(Error::UnknownCell)?;
        c.balance_quote = sub(c.balance_quote, payout)?;
        c.reserved_quote = sub(c.reserved_quote, p.maximum_payout_quote)?;
        self.positions.remove(&p.position);
        Ok(Outcome::Closed(CloseReceipt {
            position: p.position,
            member: p.intent.consent.member,
            owner: p.owner,
            cell: p.intent.cell,
            policy_revision_at_open: p.intent.consent.policy_revision,
            market_generation: p.market.generation,
            observation: observation.clone(),
            executed_slot: now,
            payout_quote: payout,
            realized_loss_quote: loss,
            released_reserve_quote: p.maximum_payout_quote,
        }))
    }

    /// Exact atom reconciliation and per-owner/per-cell liability checks.
    pub fn check_invariants(&self) -> Result<(), Error> {
        let mut holdings = self.external_out_quote;
        for (id, member) in &self.members {
            holdings = add(holdings, member.free_quote)?;
            if member.contributed_quote > member.policy.contribution_limit_quote {
                return Err(Error::Invariant);
            }
            let mut collateral = 0;
            let mut notional = 0;
            let mut count = 0u16;
            for p in self
                .positions
                .values()
                .filter(|p| p.intent.consent.member == *id)
            {
                if p.owner != member.owner
                    || p.intent.consent.member_generation != member.generation
                {
                    return Err(Error::Invariant);
                }
                collateral = add(collateral, p.intent.collateral_quote)?;
                notional = add(notional, p.entry_notional_quote)?;
                count = count.checked_add(1).ok_or(Error::Arithmetic)?;
            }
            if collateral != member.collateral_at_risk_quote
                || notional != member.open_notional_quote
                || count != member.open_positions
                || add(member.loss_and_fee_used_quote, collateral)? > member.contributed_quote
            {
                return Err(Error::Invariant);
            }
        }
        for (id, cell) in &self.cells {
            holdings = add(holdings, cell.balance_quote)?;
            let mut reserved = 0;
            for p in self.positions.values().filter(|p| p.intent.cell == *id) {
                if p.backing_owner != cell.owner
                    || !self.members.contains_key(&p.intent.consent.member)
                {
                    return Err(Error::Invariant);
                }
                if p.maximum_payout_quote
                    != add(p.intent.collateral_quote, p.intent.profit_cap_quote)?
                {
                    return Err(Error::Invariant);
                }
                reserved = add(reserved, p.maximum_payout_quote)?;
            }
            if reserved != cell.reserved_quote || reserved > cell.balance_quote {
                return Err(Error::Invariant);
            }
        }
        if holdings != self.external_in_quote
            || self
                .positions
                .values()
                .any(|p| !self.cells.contains_key(&p.intent.cell))
        {
            return Err(Error::Invariant);
        }
        Ok(())
    }
}
