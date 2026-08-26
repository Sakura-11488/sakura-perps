//! In-process SVM tests for the eleven stage-3 entry points, driven end to end.
//!
//! The risk crate's property tests already prove the arithmetic, and
//! `programs/sakura-perps/src/position.rs`'s host tests already prove the two
//! ledgers preserve I1 in isolation. What neither can reach is the thing that
//! actually broke: an instruction that **reverts**. `apply_close_ledger` called
//! by hand cannot fail the way the real one does, because the real one is
//! followed by `assert_pool_invariants` against a live `quote_vault` balance —
//! and it is that assertion firing, on a close, in a milestone with no keeper
//! liquidation, that turns a mis-booked fee into a permanently unclosable
//! position.
//!
//! So every test here is written to fail by **`Err`**, not by a wrong number.
//! Each one names the blocker it exists for and asserts the case is
//! discriminating before asserting the outcome, because a B1 test whose close
//! fee happened to be zero would pass with the bug still present.
//!
//! Harness idiom is `vault.rs`'s: LiteSVM, the compiled `.so` read from
//! `target/deploy` or `SAKURA_PERPS_SO`, a legacy SPL Token mint matching devnet
//! USDC, token balances read straight out of the account bytes, and errors
//! matched on the numeric Anchor code.

use anchor_lang::{
    AnchorDeserialize, AnchorSerialize, Discriminator, InstructionData, ToAccountMetas,
};
use litesvm::LiteSVM;
use litesvm_token::{CreateAssociatedTokenAccount, CreateMint, MintTo};
use pyth_solana_receiver_sdk::price_update::{PriceFeedMessage, PriceUpdateV2, VerificationLevel};
use sakura_perps::market::{Market, QualifiedFeed, QualifyFeedParams, RiskParams};
use sakura_perps::pool::{InitializePoolParams, Pool};
use sakura_perps::position::{
    ClosePositionParams, CloseReason, OpenPositionParams, Position, PositionClosed, SIDE_LONG,
    SIDE_SHORT,
};
use sakura_perps::{
    Exchange, InitializeExchangeParams, PauseFlags, PerpsError, BPS_DENOMINATOR,
    EMERGENCY_CLOSE_DELAY_SECONDS, MAX_ASSET_DECIMALS, MAX_BORROW_RATE_PER_HOUR,
    MAX_FUNDING_RATE_PER_HOUR, MAX_FUNDING_SENSITIVITY, MAX_RESERVE_LEVERAGE,
    MAX_SETTLE_WINDOW_SECONDS, MAX_SPREAD_BPS, MAX_TRADE_FEE_BPS,
};
use sakura_perps_risk::funding::borrow_index_delta;
use sakura_perps_risk::oracle::{diverges_beyond, MAX_EXPONENT, MIN_EXPONENT};
use sakura_perps_risk::pool::utilization_bps;
use sakura_perps_risk::position::{
    equity, execution_price, is_liquidatable, liquidation_fee, margin_requirement,
    notional_usd_ceil, trade_fee, unrealized_pnl, PriceDirection, Side,
};
use sakura_perps_risk::scale::{quote_to_usd_floor, usd_to_quote_ceil, PRICE_SCALE, USD_SCALE};
use solana_account::Account;
use solana_address::Address;
use solana_clock::Clock;
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

/// Legacy SPL Token program. Parsed rather than imported from `spl-token`,
/// whose `Pubkey` comes from a different `solana-pubkey` version and therefore
/// is not the same type as everything else here.
fn spl_token_id() -> Address {
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        .parse()
        .unwrap()
}

fn rent_sysvar_id() -> Address {
    "SysvarRent111111111111111111111111111111111"
        .parse()
        .unwrap()
}

/// Six decimals, matching devnet USDC. Deliberate: `usd_to_quote_floor` is the
/// **identity** at six decimals, so no ledger line gets a conservative rounding
/// surplus to hide an error in. Every equality below therefore has to be exact.
const COLLATERAL_DECIMALS: u8 = 6;
/// One collateral token, which at six decimals is one dollar.
const ONE: u64 = 1_000_000;
/// Base-unit decimals of the traded asset.
const ASSET_DECIMALS: u8 = 9;
/// One whole unit of the traded asset.
const ONE_UNIT: u64 = 1_000_000_000;
/// Pyth's usual crypto exponent.
const PRICE_EXPONENT: i32 = -8;

/// Cluster time the fixture pins, so staleness and the wind-down delay are
/// exact rather than a function of when the suite happens to run.
const NOW_UNIX: i64 = 1_800_000_000;
/// Cluster slot the fixture pins.
const NOW_SLOT: u64 = 100_000;

/// Liquidity the pool starts with. Large enough that no test is incidentally
/// constrained by the utilisation ceiling — that ceiling has its own tests in
/// `vault.rs` and is not what is under examination here.
const LP_LIQUIDITY: u64 = 1_000_000 * ONE;

const FEED_ID: [u8; 32] = [7u8; 32];
/// A second feed, so a position can be presented against the wrong market.
const OTHER_FEED_ID: [u8; 32] = [11u8; 32];

/// A price in whole cents, at `PRICE_SCALE`. `PRICE_SCALE` is `1e10`, so one
/// cent is `1e8`.
fn price_at(cents: u64) -> u128 {
    u128::from(cents) * (PRICE_SCALE / 100)
}

/// The Pyth mantissa for the same price at exponent `-8`: one dollar is `1e8`,
/// so one cent is `1e6`.
fn mantissa_at(cents: u64) -> i64 {
    (cents as i64) * 1_000_000
}

fn program_binary() -> Vec<u8> {
    let path = std::env::var("SAKURA_PERPS_SO")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/deploy/sakura_perps.so")
        });
    std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read the program binary at {}: {err}\n\
             Build it with `anchor build`, or point SAKURA_PERPS_SO at an \
             existing .so. These tests deliberately do not skip.",
            path.display()
        )
    })
}

fn pda(seeds: &[&[u8]]) -> Address {
    Address::find_program_address(seeds, &sakura_perps::ID).0
}

/// Assemble a `PriceUpdateV2` account exactly as the Pyth receiver writes it.
///
/// The EMA is set equal to the spot deliberately. Divergence is a **rejection**
/// at open and a **clamp** at exit, both against this number; leaving them equal
/// means a test that moves the price is exercising the settlement path rather
/// than accidentally exercising the clamp. `oracle.rs` covers the clamp itself.
fn price_update_account(
    feed_id: [u8; 32],
    cents: u64,
    publish_time: i64,
    posted_slot: u64,
) -> Account {
    price_update_account_with_ema(feed_id, cents, cents, publish_time, posted_slot)
}

/// The same account with the EMA held apart from the spot.
///
/// `open_position` is the one leg that **rejects** on divergence rather than
/// clamping, and it measures that divergence against this field. With spot and
/// EMA equal — which is what every other test in this file wants, so that a
/// price move exercises settlement rather than the clamp — the divergence gate
/// is never approached from either side and could be deleted unnoticed.
fn price_update_account_with_ema(
    feed_id: [u8; 32],
    cents: u64,
    ema_cents: u64,
    publish_time: i64,
    posted_slot: u64,
) -> Account {
    let mantissa = mantissa_at(cents);
    let update = PriceUpdateV2 {
        write_authority: Address::new_unique(),
        verification_level: VerificationLevel::Full,
        price_message: PriceFeedMessage {
            feed_id,
            price: mantissa,
            // Zero, so the execution price is the mid adjusted by the spread
            // alone and every expected value below is exact. The confidence
            // adjustment has its own coverage in `oracle.rs`.
            conf: 0,
            exponent: PRICE_EXPONENT,
            publish_time,
            prev_publish_time: publish_time - 1,
            ema_price: mantissa_at(ema_cents),
            ema_conf: 0,
        },
        posted_slot,
    };

    let mut data = PriceUpdateV2::DISCRIMINATOR.to_vec();
    update
        .serialize(&mut data)
        .expect("PriceUpdateV2 serialises");

    Account {
        lamports: 1_000_000_000,
        data,
        // Ownership is what makes this a Pyth account rather than 200 bytes of
        // wishful thinking. `Account<PriceUpdateV2>` checks it.
        owner: pyth_solana_receiver_sdk::ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// Feed parameters every test shares. The sanity band is wide because no test
/// here is about the band; `oracle.rs` owns that.
fn feed_params(feed_id: [u8; 32]) -> QualifyFeedParams {
    QualifyFeedParams {
        feed_id,
        expected_exponent: PRICE_EXPONENT,
        asset_decimals: ASSET_DECIMALS,
        min_price: PRICE_SCALE / 1_000,
        max_price: 1_000_000 * PRICE_SCALE,
        trading_max_age_seconds: 30,
        trading_max_age_slots: 150,
        trading_max_future_skew_seconds: 5,
        trading_max_confidence_bps: 100,
        liquidation_max_age_seconds: 120,
        liquidation_max_age_slots: 400,
        liquidation_max_future_skew_seconds: 10,
        liquidation_max_confidence_bps: 500,
        max_divergence_bps: 500,
    }
}

/// An activated market that passes all ten of `set_risk_params`'s validations.
///
/// Borrow and funding are switched off so every equity figure below is a
/// function of the price alone. The one test that needs accrual turns the borrow
/// rate back on explicitly, and says so.
/// The keeper's cut of a liquidation fee in this fixture, in bps.
///
/// 20%: large enough that a rounding error or a dropped payment is visible in an
/// assertion, small enough to stay under `MAX_KEEPER_FEE_SHARE_BPS`.
const KEEPER_FEE_SHARE_BPS: u16 = 2_000;

fn active_params() -> RiskParams {
    RiskParams {
        initial_margin_bps: 1_000,
        maintenance_margin_bps: 500,
        liquidation_fee_bps: 100,
        max_profit_bps: 4_000,
        spread_bps: 10,
        open_fee_bps: 10,
        close_fee_bps: 10,
        max_oi_usd: 10_000_000 * USD_SCALE,
        max_oracle_drift_bps: 40,
        min_position_size_base: 1,
        min_notional_usd: 1,
        min_collateral_usd: 1,
        borrow_rate_per_hour: 0,
        funding_sensitivity: 0,
        funding_cap_per_hour: 0,
        max_settle_window_seconds: 3_600,
        min_settle_interval_seconds: 60,
    }
}

/// The same block with `max_oi_usd == 0`, which **is** the quarantine.
fn quarantined_params(base: RiskParams) -> RiskParams {
    RiskParams {
        max_oi_usd: 0,
        ..base
    }
}

/// A slippage bound that cannot bind, in whichever direction the side reads it:
/// a ceiling for a long, which pays up, and a floor for a short, which receives
/// down.
fn unbounded_limit(side: u8) -> u128 {
    if side == SIDE_LONG {
        u128::MAX
    } else {
        1
    }
}

/// An associated token account for `owner`, funded by the mint authority.
fn funded_token_account(
    svm: &mut LiteSVM,
    mint: &Address,
    mint_authority: &Keypair,
    owner: &Keypair,
    amount: u64,
) -> Address {
    let account = CreateAssociatedTokenAccount::new(svm, owner, mint)
        .owner(&owner.pubkey())
        .send()
        .expect("token account");
    if amount > 0 {
        MintTo::new(svm, mint_authority, mint, &account, amount)
            .send()
            .expect("fund token account");
    }
    account
}

/// Everything a position test needs, with one activated market already listed.
struct Fixture {
    svm: LiteSVM,
    admin: Keypair,
    lp: Keypair,
    trader: Keypair,
    collateral_mint: Address,
    exchange: Address,
    pool: Address,
    quote_vault: Address,
    share_mint: Address,
    pool_share_account: Address,
    lp_token_account: Address,
    lp_share_account: Address,
    trader_token_account: Address,
    admin_token_account: Address,
    feed: Address,
    market: Address,
    price_update: Address,
    /// Cluster time and slot, mirrored here so `set_price` can write an update
    /// that is fresh against whatever the clock has been advanced to.
    now_unix: i64,
    now_slot: u64,
}

impl Fixture {
    fn new(risk: RiskParams) -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(sakura_perps::ID, &program_binary())
            .expect("program loads");

        // Pin the clock. Every staleness bound and the wind-down delay are
        // measured against this, so the assertions are exact rather than a
        // function of when the suite ran.
        let mut clock: Clock = svm.get_sysvar();
        clock.unix_timestamp = NOW_UNIX;
        clock.slot = NOW_SLOT;
        svm.set_sysvar(&clock);

        let admin = Keypair::new();
        let lp = Keypair::new();
        let trader = Keypair::new();
        for who in [&admin, &lp, &trader] {
            svm.airdrop(&who.pubkey(), 100 * 1_000_000_000).unwrap();
        }

        // No freeze authority: the exchange refuses freezable collateral unless
        // the admin opts in, and leaving the opt-in closed keeps that guard on
        // its default path. `collateral_guard.rs` owns the other direction.
        let collateral_mint = CreateMint::new(&mut svm, &admin)
            .decimals(COLLATERAL_DECIMALS)
            .send()
            .expect("collateral mint");

        let lp_token_account =
            funded_token_account(&mut svm, &collateral_mint, &admin, &lp, 10 * LP_LIQUIDITY);
        let trader_token_account =
            funded_token_account(&mut svm, &collateral_mint, &admin, &trader, 100_000 * ONE);
        // Funded, so a test that tries to redirect a payout here would visibly
        // succeed if the constraint were missing, rather than failing on a
        // missing account and proving nothing.
        let admin_token_account =
            funded_token_account(&mut svm, &collateral_mint, &admin, &admin, ONE);

        let mut fixture = Self {
            svm,
            admin,
            lp,
            trader,
            collateral_mint,
            exchange: pda(&[b"exchange"]),
            pool: pda(&[b"pool"]),
            quote_vault: pda(&[b"quote_vault"]),
            share_mint: pda(&[b"share_mint"]),
            pool_share_account: pda(&[b"pool_shares"]),
            lp_token_account,
            lp_share_account: Address::default(),
            trader_token_account,
            admin_token_account,
            feed: pda(&[b"feed", FEED_ID.as_ref()]),
            market: pda(&[b"market", FEED_ID.as_ref()]),
            price_update: Address::new_unique(),
            now_unix: NOW_UNIX,
            now_slot: NOW_SLOT,
        };

        fixture.initialize_exchange();
        // The exchange is created with every flag set; nothing can happen until
        // they are cleared.
        fixture.set_pause_flags(0);
        fixture.initialize_pool();

        fixture.lp_share_account =
            CreateAssociatedTokenAccount::new(&mut fixture.svm, &fixture.lp, &fixture.share_mint)
                .owner(&fixture.lp.pubkey())
                .send()
                .expect("lp share account");
        fixture.lp_deposit(LP_LIQUIDITY);

        // $100.00 to start. Every price below is expressed in whole cents.
        fixture.write_price(fixture.price_update, FEED_ID, 10_000);
        fixture.qualify_feed(FEED_ID).expect("qualify feed");
        fixture.create_market(FEED_ID).expect("create market");
        fixture
            .set_risk_params(fixture.market, risk)
            .expect("activate market");

        fixture
    }

    // ── plumbing ────────────────────────────────────────────────────────────

    /// Sign and send, returning the transaction metadata so a test can read the
    /// events the instruction emitted.
    ///
    /// The blockhash is expired first. Two attempts at the same instruction
    /// against the same accounts — which several tests below make deliberately,
    /// once expecting failure and once success — produce byte-identical
    /// transactions, and the runtime rejects the second as `AlreadyProcessed`.
    /// That is a deduplication artifact that would masquerade as the property
    /// under test.
    fn submit(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<(Vec<String>, u64), TransactionError> {
        self.svm.expire_blockhash();
        let payer = signers[0];
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            signers,
            self.svm.latest_blockhash(),
        );
        self.svm
            .send_transaction(transaction)
            .map(|meta| (meta.logs, meta.compute_units_consumed))
            .map_err(|failed| failed.err)
    }

    fn send_meta(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<Vec<String>, TransactionError> {
        self.submit(instruction, signers).map(|(logs, _)| logs)
    }

    /// Compute units the runtime actually charged for one instruction.
    ///
    /// The metadata carried this all along and `send_meta` was discarding it,
    /// which is why spec §9.11 could go unanswered while the suite looked
    /// thorough — nothing was measuring the one number it asks for.
    fn send_cu(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<u64, TransactionError> {
        self.submit(instruction, signers).map(|(_, cu)| cu)
    }

    fn send(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<(), TransactionError> {
        self.send_meta(instruction, signers).map(|_| ())
    }

    fn initialize_exchange(&mut self) {
        let instruction = Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::InitializeExchange {
                admin: self.admin.pubkey(),
                exchange: self.exchange,
                collateral_mint: self.collateral_mint,
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::InitializeExchange {
                params: InitializeExchangeParams {
                    fee_recipient: self.admin.pubkey(),
                    protocol_fee_share_bps: 1_000,
                    // Deliberately non-zero. A keeper share of zero would let a
                    // liquidation test pass while paying the keeper nothing,
                    // which is exactly the bug worth catching.
                    keeper_fee_share_bps: KEEPER_FEE_SHARE_BPS,
                    allow_freezable_collateral: false,
                },
            }
            .data(),
        };
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
            .expect("initialize exchange");
    }

    fn set_pause_flags_ix(&self, flags: u64) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::SetPauseFlags {
                admin: self.admin.pubkey(),
                exchange: self.exchange,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::SetPauseFlags { flags }.data(),
        }
    }

    fn set_pause_flags(&mut self, flags: u64) {
        let instruction = self.set_pause_flags_ix(flags);
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin]).expect("set pause flags");
    }

    fn initialize_pool(&mut self) {
        let instruction = Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::InitializePool {
                admin: self.admin.pubkey(),
                exchange: self.exchange,
                pool: self.pool,
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                share_mint: self.share_mint,
                pool_share_account: self.pool_share_account,
                token_program: spl_token_id(),
                system_program: anchor_lang::system_program::ID,
                rent: rent_sysvar_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::InitializePool {
                params: InitializePoolParams {
                    deposit_fee_bps: 0,
                    withdraw_fee_bps: 0,
                    withdraw_delay_seconds: 60,
                    max_utilization_bps: sakura_perps::M5_MAX_UTILIZATION_BPS,
                    max_aum_quote: 100 * LP_LIQUIDITY,
                },
            }
            .data(),
        };
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin]).expect("initialize pool");
    }

    fn lp_deposit(&mut self, amount: u64) {
        let instruction = Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::LpDeposit {
                depositor: self.lp.pubkey(),
                exchange: self.exchange,
                pool: self.pool,
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                share_mint: self.share_mint,
                depositor_token_account: self.lp_token_account,
                depositor_share_account: self.lp_share_account,
                pool_share_account: self.pool_share_account,
                token_program: spl_token_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::LpDeposit {
                amount,
                min_shares_out: 1,
            }
            .data(),
        };
        let lp = self.lp.insecure_clone();
        self.send(instruction, &[&lp]).expect("lp deposit");
    }

    // ── markets ─────────────────────────────────────────────────────────────

    fn write_price(&mut self, at: Address, feed_id: [u8; 32], cents: u64) {
        let account = price_update_account(feed_id, cents, self.now_unix, self.now_slot);
        self.svm.set_account(at, account).expect("write price");
    }

    /// Move the market's pinned price account to a new level, fresh on both
    /// clocks. The account address never changes, so `market.price_update`
    /// stays satisfied — this is a price move, not an oracle substitution.
    fn set_price(&mut self, cents: u64) {
        let at = self.price_update;
        self.write_price(at, FEED_ID, cents);
    }

    /// Move the spot while holding the EMA somewhere else, which is the only
    /// way to approach `open_position`'s divergence gate from either side.
    fn set_price_with_ema(&mut self, cents: u64, ema_cents: u64) {
        let account =
            price_update_account_with_ema(FEED_ID, cents, ema_cents, self.now_unix, self.now_slot);
        let at = self.price_update;
        self.svm.set_account(at, account).expect("write price");
    }

    fn qualify_feed_ix(&self, feed_id: [u8; 32], params: QualifyFeedParams) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::QualifyFeed {
                exchange: self.exchange,
                admin: self.admin.pubkey(),
                // Derived from the **params**, not from the account key: the
                // `feed` PDA's seeds are `[b"feed", params.feed_id]`, so a
                // validation row that perturbs `feed_id` must move the PDA
                // with it or it fails on the seeds instead.
                feed: pda(&[b"feed", params.feed_id.as_ref()]),
                price_update: if feed_id == FEED_ID {
                    self.price_update
                } else {
                    self.other_price_update(feed_id)
                },
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::QualifyFeed { params }.data(),
        }
    }

    fn qualify_feed(&mut self, feed_id: [u8; 32]) -> Result<(), TransactionError> {
        self.qualify_feed_with(feed_id, feed_params(feed_id))
    }

    fn qualify_feed_with(
        &mut self,
        feed_id: [u8; 32],
        params: QualifyFeedParams,
    ) -> Result<(), TransactionError> {
        let instruction = self.qualify_feed_ix(feed_id, params);
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
    }

    /// Deterministic address for a second feed's price account, so it can be
    /// derived rather than threaded through the fixture.
    fn other_price_update(&self, feed_id: [u8; 32]) -> Address {
        Address::new_from_array(feed_id)
    }

    fn create_market_ix(&self, feed_id: [u8; 32]) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::CreateMarket {
                exchange: self.exchange,
                payer: self.trader.pubkey(),
                feed: pda(&[b"feed", feed_id.as_ref()]),
                market: pda(&[b"market", feed_id.as_ref()]),
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::CreateMarket {}.data(),
        }
    }

    fn create_market(&mut self, feed_id: [u8; 32]) -> Result<(), TransactionError> {
        let instruction = self.create_market_ix(feed_id);
        // Permissionless: the trader pays the rent and receives no authority.
        let trader = self.trader.insecure_clone();
        self.send(instruction, &[&trader])
    }

    fn set_risk_params_ix(&self, market: Address, params: RiskParams) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::SetRiskParams {
                exchange: self.exchange,
                admin: self.admin.pubkey(),
                market,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::SetRiskParams { params }.data(),
        }
    }

    fn set_risk_params(
        &mut self,
        market: Address,
        params: RiskParams,
    ) -> Result<(), TransactionError> {
        let instruction = self.set_risk_params_ix(market, params);
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
    }

    fn set_pool_limits_ix(&self, max_aum_quote: u64, max_utilization_bps: u16) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::SetPoolLimits {
                exchange: self.exchange,
                admin: self.admin.pubkey(),
                pool: self.pool,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::SetPoolLimits {
                max_aum_quote,
                max_utilization_bps,
            }
            .data(),
        }
    }

    fn set_pool_limits(
        &mut self,
        max_aum_quote: u64,
        max_utilization_bps: u16,
    ) -> Result<(), TransactionError> {
        let instruction = self.set_pool_limits_ix(max_aum_quote, max_utilization_bps);
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
    }

    fn set_feed_revoked_ix(&self, feed: Address, revoked: bool) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::SetFeedRevoked {
                exchange: self.exchange,
                admin: self.admin.pubkey(),
                feed,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::SetFeedRevoked { revoked }.data(),
        }
    }

    fn set_feed_revoked(&mut self, revoked: bool) {
        let feed = self.feed;
        self.set_feed_revoked_for(feed, revoked)
            .expect("set feed revoked");
    }

    fn set_feed_revoked_for(
        &mut self,
        feed: Address,
        revoked: bool,
    ) -> Result<(), TransactionError> {
        let instruction = self.set_feed_revoked_ix(feed, revoked);
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
    }

    fn settle_market(&mut self) -> Result<(), TransactionError> {
        let instruction = Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::SettleMarket {
                pool: self.pool,
                market: self.market,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::SettleMarket {}.data(),
        };
        // Permissionless: signed by whoever pays the fee, and nothing else.
        let anyone = self.lp.insecure_clone();
        self.send(instruction, &[&anyone])
    }

    fn refresh_market_price_ix_for(&self, market: Address, price_update: Address) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::RefreshMarketPrice {
                market,
                price_update,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::RefreshMarketPrice {}.data(),
        }
    }

    fn refresh_market_price_ix(&self) -> Instruction {
        self.refresh_market_price_ix_for(self.market, self.price_update)
    }

    fn refresh_market_price(&mut self) -> Result<(), TransactionError> {
        let instruction = self.refresh_market_price_ix();
        // No signer appears in the account list at all; the fee payer is a
        // stranger, which is the point of the instruction.
        let stranger = self.lp.insecure_clone();
        self.send(instruction, &[&stranger])
    }

    /// Drive a *second* market's reference, so a substitution test can give the
    /// impostor market a price worth stealing.
    fn refresh_market_price_for(
        &mut self,
        market: Address,
        price_update: Address,
    ) -> Result<(), TransactionError> {
        let instruction = self.refresh_market_price_ix_for(market, price_update);
        let stranger = self.lp.insecure_clone();
        self.send(instruction, &[&stranger])
    }

    // ── positions ───────────────────────────────────────────────────────────

    fn position_key(&self) -> Address {
        self.position_key_for(self.trader.pubkey())
    }

    fn position_key_for(&self, owner: Address) -> Address {
        pda(&[b"position", self.market.as_ref(), owner.as_ref()])
    }

    fn open_ix_for(
        &self,
        owner: Address,
        owner_token_account: Address,
        side: u8,
        size_base: u64,
        collateral: u64,
        limit_price: u128,
    ) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::OpenPosition {
                exchange: self.exchange,
                pool: self.pool,
                market: self.market,
                feed: self.feed,
                price_update: self.price_update,
                owner,
                position: self.position_key_for(owner),
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                owner_token_account,
                token_program: spl_token_id(),
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::OpenPosition {
                params: OpenPositionParams {
                    side,
                    size_base,
                    collateral_deposited_quote: collateral,
                    limit_price,
                },
            }
            .data(),
        }
    }

    fn open_ix(&self, side: u8, size_base: u64, collateral: u64) -> Instruction {
        self.open_ix_for(
            self.trader.pubkey(),
            self.trader_token_account,
            side,
            size_base,
            collateral,
            // A ceiling for a long and a floor for a short, so neither
            // direction is bounded by anything but the market itself.
            unbounded_limit(side),
        )
    }

    fn open(&mut self, side: u8, size_base: u64, collateral: u64) -> Result<(), TransactionError> {
        let instruction = self.open_ix(side, size_base, collateral);
        let trader = self.trader.insecure_clone();
        self.send(instruction, &[&trader])
    }

    /// A funded trader who holds no position yet.
    ///
    /// Every test that expects `open_position` to be **refused** has to use one
    /// of these rather than the fixture's own trader. Anchor constructs `init`
    /// accounts while it is still walking the account list, and the deferred
    /// `constraint = …` and `address = …` checks run after that — so an open
    /// against a `[b"position", market, owner]` PDA that already exists fails
    /// with the System Program's `AccountAlreadyInUse` (custom error `0`)
    /// before the constraint under test is ever evaluated. That failure looks
    /// like a refusal and proves nothing.
    fn newcomer(&mut self) -> (Keypair, Address) {
        let who = Keypair::new();
        self.svm.airdrop(&who.pubkey(), 10_000_000_000).unwrap();
        let admin = self.admin.insecure_clone();
        let tokens = funded_token_account(
            &mut self.svm,
            &self.collateral_mint,
            &admin,
            &who,
            100_000 * ONE,
        );
        (who, tokens)
    }

    /// A funded keypair holding no position, no pool shares and no authority
    /// whatsoever — the account every admin-gated instruction must refuse.
    ///
    /// Funded because an unfunded fee payer fails before the program is even
    /// entered, which would let an authorisation test pass for the wrong
    /// reason.
    fn stranger(&mut self) -> Keypair {
        let who = Keypair::new();
        self.svm.airdrop(&who.pubkey(), 10_000_000_000).unwrap();
        who
    }

    fn close_ix(&self) -> Instruction {
        // The worst price the caller accepts: a floor for a long and a ceiling
        // for a short. Both unbounded, so slippage never masks the outcome
        // under test — the one test that is about slippage passes its own.
        let limit_price = match self.position_state().side {
            SIDE_LONG => 1,
            _ => u128::MAX,
        };
        self.close_ix_with(limit_price)
    }

    fn close_ix_with(&self, limit_price: u128) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::ClosePosition {
                exchange: self.exchange,
                pool: self.pool,
                market: self.market,
                price_update: self.price_update,
                owner: self.trader.pubkey(),
                position: self.position_key(),
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                owner_token_account: self.trader_token_account,
                token_program: spl_token_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::ClosePosition {
                params: ClosePositionParams { limit_price },
            }
            .data(),
        }
    }

    fn close(&mut self) -> Result<Vec<String>, TransactionError> {
        let instruction = self.close_ix();
        let trader = self.trader.insecure_clone();
        self.send_meta(instruction, &[&trader])
    }

    fn admin_settle_ix(&self) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::AdminSettlePosition {
                exchange: self.exchange,
                admin: self.admin.pubkey(),
                pool: self.pool,
                market: self.market,
                price_update: self.price_update,
                owner: self.trader.pubkey(),
                position: self.position_key(),
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                owner_token_account: self.trader_token_account,
                token_program: spl_token_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::AdminSettlePosition {}.data(),
        }
    }

    fn admin_settle(&mut self) -> Result<Vec<String>, TransactionError> {
        let instruction = self.admin_settle_ix();
        let admin = self.admin.insecure_clone();
        self.send_meta(instruction, &[&admin])
    }

    fn emergency_close_ix(&self) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::EmergencyClosePosition {
                exchange: self.exchange,
                admin: self.admin.pubkey(),
                pool: self.pool,
                market: self.market,
                owner: self.trader.pubkey(),
                position: self.position_key(),
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                owner_token_account: self.trader_token_account,
                token_program: spl_token_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::EmergencyClosePosition {}.data(),
        }
    }

    fn emergency_close(&mut self) -> Result<Vec<String>, TransactionError> {
        let instruction = self.emergency_close_ix();
        let admin = self.admin.insecure_clone();
        self.send_meta(instruction, &[&admin])
    }

    // ── state ───────────────────────────────────────────────────────────────

    fn pool_state(&self) -> Pool {
        let account = self.svm.get_account(&self.pool).expect("pool exists");
        AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("pool decodes")
    }

    fn exchange_state(&self) -> Exchange {
        let account = self
            .svm
            .get_account(&self.exchange)
            .expect("exchange exists");
        AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("exchange decodes")
    }

    fn feed_state(&self) -> QualifiedFeed {
        let account = self.svm.get_account(&self.feed).expect("feed exists");
        AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("feed decodes")
    }

    fn market_state(&self) -> Market {
        self.market_state_at(self.market)
    }

    fn market_state_at(&self, key: Address) -> Market {
        let account = self.svm.get_account(&key).expect("market exists");
        AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("market decodes")
    }

    /// Write the market account back with `mutate` applied to its decoded state.
    ///
    /// Reaching a state no instruction can produce by writing the account
    /// directly is the established idiom — `vault.rs` zeroes an escrow's token
    /// amount the same way. It is used once here, to clear `last_good_price`:
    /// four instructions set that field and none clears it, so the legacy
    /// zero-reference case the fallback exists for is otherwise unreachable.
    ///
    /// The discriminator is carried across untouched, so the account stays a
    /// `Market` as far as `try_deserialize` is concerned.
    fn write_market<F: FnOnce(&mut Market)>(&mut self, mutate: F) {
        let mut account = self.svm.get_account(&self.market).expect("market exists");
        let mut market: Market =
            AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("market decodes");
        mutate(&mut market);

        let mut data = account.data[..8].to_vec();
        market.serialize(&mut data).expect("market serialises");
        assert!(
            data.len() <= account.data.len(),
            "the rewritten market ({} bytes) must fit the account it came from ({})",
            data.len(),
            account.data.len()
        );
        data.resize(account.data.len(), 0);

        account.data = data;
        self.svm
            .set_account(self.market, account)
            .expect("write market");
    }

    fn position_state(&self) -> Position {
        let account = self
            .svm
            .get_account(&self.position_key())
            .expect("position exists");
        AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("position decodes")
    }

    fn position_exists(&self) -> bool {
        self.svm
            .get_account(&self.position_key())
            .is_some_and(|account| !account.data.is_empty())
    }

    /// SPL token account layout: mint(32), owner(32), amount(8, little endian).
    /// Read directly rather than pulling in a token crate whose `Pubkey` belongs
    /// to a different `solana-pubkey` version.
    fn token_balance(&self, account: Address) -> u64 {
        let raw = self
            .svm
            .get_account(&account)
            .expect("token account exists");
        u64::from_le_bytes(raw.data[64..72].try_into().expect("long enough"))
    }

    /// Exactly the sum `assert_vault_solvent` compares against the vault
    /// balance. `reserved_quote` is deliberately absent: a reserve is a claim
    /// against liquidity-provider equity, not a liability on top of it.
    fn liabilities(&self) -> u64 {
        let pool = self.pool_state();
        pool.quote_deposited + pool.locked_quote + pool.pending_protocol_fees
    }

    /// I1, to the unit and as an **equality**.
    ///
    /// The pool is only ever funded through `lp_deposit` here, with no fees and
    /// no donation, so the vault holds exactly what is recorded against it and
    /// nothing more. That is the case worth asserting: with no surplus there is
    /// nowhere for a mis-booked fee to hide, and `>=` would let a whole class of
    /// over-crediting pass.
    fn assert_i1(&self, context: &str) {
        assert_eq!(
            self.token_balance(self.quote_vault),
            self.liabilities(),
            "I1 must hold to the unit ({context}): vault {} vs liabilities {}",
            self.token_balance(self.quote_vault),
            self.liabilities(),
        );
    }

    /// Move the cluster clock. Negative deltas are the point of one test.
    fn advance(&mut self, seconds: i64, slots: u64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp += seconds;
        clock.slot += slots;
        self.now_unix = clock.unix_timestamp;
        self.now_slot = clock.slot;
        self.svm.set_sysvar(&clock);
    }
}

/// Point every meta that names `from` at `to` instead.
///
/// The established substitution idiom — build the instruction through its own
/// constructor, then swap one account — with the one guard that makes it
/// trustworthy: a substitution that matched nothing leaves a *correct*
/// instruction behind, which then succeeds or fails for reasons that have
/// nothing to do with the constraint under test.
fn substitute(instruction: &mut Instruction, from: Address, to: Address) {
    let mut matched = 0usize;
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == from {
            meta.pubkey = to;
            matched += 1;
        }
    }
    assert!(
        matched > 0,
        "nothing to substitute: {from} appears in no account meta"
    );
}

fn expect_error(result: Result<(), TransactionError>, expected: PerpsError) {
    expect_error_at("", result, expected);
}

/// `expect_error` carrying a row label.
///
/// The table-driven validation tests below have fifteen rows sharing four error
/// variants; a bare `expected InvalidRiskParameters, got Ok(())` does not say
/// which of the five fields mapped to that variant stopped being checked.
fn expect_error_at(context: &str, result: Result<(), TransactionError>, expected: PerpsError) {
    let code = expected as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
    match result {
        Err(TransactionError::InstructionError(0, InstructionError::Custom(actual))) => {
            assert_eq!(
                actual, code,
                "{context}expected {expected:?} ({code}), got {actual}"
            );
        }
        other => panic!("{context}expected {expected:?}, got {other:?}"),
    }
}

// ── events ──────────────────────────────────────────────────────────────────

/// Minimal standard-alphabet base64 decoder.
///
/// Anchor writes an event as `Program data: <base64>`, and the alternative to
/// twelve lines here is a dependency this crate does not otherwise need. The
/// events are worth reading: `close_fee_quote` and `liquidation_fee_quote` on
/// `PositionClosed` are documented to be the amounts the vault **retained**, and
/// asserting that against the pool's own accounting is exactly the B1 property.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut accumulator: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for byte in input.trim().bytes() {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|c| *c == byte)? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Some(out)
}

/// The first event of type `E` in a transaction's logs.
fn event<E: Discriminator + AnchorDeserialize>(logs: &[String]) -> E {
    for line in logs {
        let Some(payload) = line.strip_prefix("Program data: ") else {
            continue;
        };
        let Some(bytes) = base64_decode(payload) else {
            continue;
        };
        if bytes.len() < 8 || bytes[..8] != *E::DISCRIMINATOR {
            continue;
        }
        if let Ok(decoded) = E::deserialize(&mut &bytes[8..]) {
            return decoded;
        }
    }
    panic!("no matching event in logs: {logs:#?}");
}

// ── shared scenario helpers ─────────────────────────────────────────────────

/// One hundred units of the asset. At $100 that is $10,000 of notional, which is
/// the size the specification's late-liquidation case is stated at.
const BIG_SIZE: u64 = 100 * ONE_UNIT;

/// What the close path will value this position at, recomputed here from the
/// accounts rather than from the numbers the test fed in.
///
/// Deriving the expectation from on-chain state is what makes these assertions
/// discriminating rather than tautological: the position's `entry_price`,
/// `collateral_quote` and `close_fee_bps` are whatever `open_position` actually
/// wrote, and a change to that instruction moves the expectation with it —
/// while a change to the **close** path breaks the comparison, which is the
/// half under test.
struct Expected {
    equity_usd: i128,
    exit_price: u128,
    current_notional_usd: u128,
    close_fee_usd: u128,
    close_fee_quote_unclamped: u128,
    gross_payout_quote: u128,
}

fn expected_close(fixture: &Fixture, mid: u128) -> Expected {
    let position = fixture.position_state();
    let market = fixture.market_state();
    let side = if position.side == SIDE_LONG {
        Side::Long
    } else {
        Side::Short
    };

    let exit_price = execution_price(
        side,
        PriceDirection::Close,
        mid,
        0,
        // The position's snapshot, never the market's live value.
        position.spread_bps,
    )
    .unwrap();
    let pnl = unrealized_pnl(
        side,
        u128::from(position.size_base),
        position.entry_price,
        exit_price,
        market.asset_decimals,
    )
    .unwrap();
    // Borrow and funding are switched off in every market these helpers are
    // used with, so equity is collateral plus PnL exactly.
    let equity_usd = equity(
        quote_to_usd_floor(u128::from(position.collateral_quote), COLLATERAL_DECIMALS).unwrap(),
        pnl,
        0,
        0,
    )
    .unwrap();

    let exit_notional_usd = notional_usd_ceil(
        u128::from(position.size_base),
        exit_price,
        market.asset_decimals,
    )
    .unwrap();
    let current_notional_usd =
        notional_usd_ceil(u128::from(position.size_base), mid, market.asset_decimals).unwrap();
    let close_fee_usd = trade_fee(exit_notional_usd, position.close_fee_bps).unwrap();

    let gross_payout_quote = if equity_usd <= 0 {
        0
    } else {
        sakura_perps_risk::scale::usd_to_quote_floor(equity_usd as u128, COLLATERAL_DECIMALS)
            .unwrap()
            .min(u128::from(position.collateral_quote) + u128::from(position.reserve_quote))
    };

    Expected {
        equity_usd,
        exit_price,
        current_notional_usd,
        close_fee_usd,
        close_fee_quote_unclamped: usd_to_quote_ceil(close_fee_usd, COLLATERAL_DECIMALS).unwrap(),
        gross_payout_quote,
    }
}

/// Fee revenue the pool has recorded, in both places it lands.
fn fee_revenue(fixture: &Fixture) -> (u64, u64) {
    let pool = fixture.pool_state();
    (pool.pending_protocol_fees, pool.quote_deposited)
}

// ════════════════════════════════════════════════════════════════════════════
// B1 — the close fee that is booked must be the settlement's clamped output.
// ════════════════════════════════════════════════════════════════════════════

/// **B1, the zero-equity half.** The most important test in the milestone.
///
/// `settle_close` takes `close_fee_usd` as an input and returns
/// `close_fee_quote` as a clamped output — **zero** whenever equity is
/// non-positive. Booking the input instead credits the pool fee revenue the
/// vault never received, which pushes
/// `quote_deposited + locked_quote + pending_protocol_fees` above
/// `quote_vault.amount`; `assert_pool_invariants` then reverts the close. M5
/// ships no keeper liquidation, so there is no second way out: the position
/// becomes permanently unclosable.
///
/// This test therefore fails as an `Err` from `close_position`, not as a wrong
/// number, which is exactly the failure the bug produces.
#[test]
fn a_close_at_non_positive_equity_succeeds_and_books_no_fee() {
    let mut fixture = Fixture::new(active_params());
    fixture.assert_i1("after the liquidity deposit");

    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open a $10,000 long against $1,100 of collateral");
    fixture.assert_i1("after the open");
    let (pending_after_open, deposited_after_open) = fee_revenue(&fixture);
    let vault_after_open = fixture.token_balance(fixture.quote_vault);
    let position = fixture.position_state();

    // The price collapses far enough to wipe the collateral out. The EMA moves
    // with it, so the divergence clamp is not what is being exercised.
    fixture.set_price(8_500);
    let expected = expected_close(&fixture, price_at(8_500));

    // Non-vacuity, both halves. A fee really was computed — a test where the
    // input happened to be zero would pass with the bug still present — and the
    // position really is underwater.
    assert!(
        expected.close_fee_usd > 0 && expected.close_fee_quote_unclamped > 0,
        "the close fee input must be non-zero or this test proves nothing"
    );
    assert!(
        expected.equity_usd <= 0,
        "equity must be non-positive for the zero-fee branch: {}",
        expected.equity_usd
    );

    let logs = fixture
        .close()
        .expect("a close at non-positive equity must succeed");

    let closed: PositionClosed = event(&logs);
    assert_eq!(closed.exit_price, expected.exit_price);
    assert_eq!(closed.gross_payout_quote, 0, "nothing to pay out");
    assert_eq!(
        closed.close_fee_quote, 0,
        "the settlement's clamped output is zero, and that is what must be reported"
    );
    assert_eq!(closed.liquidation_fee_quote, 0);
    assert_eq!(closed.net_payout_quote, 0);
    assert_eq!(
        closed.bad_debt_usd,
        expected.equity_usd.unsigned_abs(),
        "the shortfall is recorded rather than absorbed silently"
    );

    // The pool booked no fee at all. `quote_deposited` still moves, by the
    // trader's whole collateral, because the pool kept every cent of it — that
    // is the loss being absorbed, not revenue.
    let (pending_after_close, deposited_after_close) = fee_revenue(&fixture);
    assert_eq!(
        pending_after_close, pending_after_open,
        "no protocol fee may be booked on a close that collected none"
    );
    assert_eq!(
        deposited_after_close - deposited_after_open,
        position.collateral_quote,
        "liquidity providers take the collateral and nothing more"
    );

    // The actual vault balance, not merely the invariant derived from it:
    // nothing was paid out, so nothing may have left.
    assert_eq!(
        fixture.token_balance(fixture.quote_vault),
        vault_after_open,
        "a zero payout must move no tokens at all"
    );
    fixture.assert_i1("after the close");
    assert!(!fixture.position_exists(), "the position must be gone");
}

/// **B1, the clamp-binds half.** Equity is positive but smaller than the fee.
///
/// `min(ceil(fee), gross_payout)` is the other half of the same output, and it
/// is reachable on any ordinary late exit: the fee is a fraction of **notional**
/// while the payout is what is left of **equity**, and those diverge without
/// limit. Here the computed fee is roughly seventy times the payout.
#[test]
fn a_close_whose_fee_exceeds_the_payout_books_only_the_payout() {
    // The maximum legal close fee, so the clamp binds by a wide margin rather
    // than by a rounding unit.
    let mut fixture = Fixture::new(RiskParams {
        close_fee_bps: 500,
        ..active_params()
    });

    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");
    fixture.assert_i1("after the open");
    let (pending_after_open, deposited_after_open) = fee_revenue(&fixture);
    let vault_after_open = fixture.token_balance(fixture.quote_vault);
    let position = fixture.position_state();

    // Down 10.65%: enough to leave a few dollars of equity and nothing like
    // enough to pay a 5% fee on ~$8,900 of exit notional.
    fixture.set_price(8_935);
    let expected = expected_close(&fixture, price_at(8_935));

    assert!(
        expected.equity_usd > 0,
        "this half needs positive equity, not the zero branch: {}",
        expected.equity_usd
    );
    assert!(
        expected.close_fee_quote_unclamped > expected.gross_payout_quote,
        "the clamp must genuinely bind: fee {} vs gross {}",
        expected.close_fee_quote_unclamped,
        expected.gross_payout_quote
    );

    let logs = fixture.close().expect("a fee-clamped close must succeed");
    let closed: PositionClosed = event(&logs);

    let gross = u64::try_from(expected.gross_payout_quote).unwrap();
    assert_eq!(closed.gross_payout_quote, gross);
    assert_eq!(
        closed.close_fee_quote, gross,
        "the fee is clamped to the payout, never to what it was computed as"
    );
    assert_eq!(closed.net_payout_quote, 0);
    assert_eq!(
        closed.close_fee_quote + closed.liquidation_fee_quote + closed.net_payout_quote,
        closed.gross_payout_quote,
        "the four amounts must re-sum"
    );

    // Fee revenue rose by exactly the clamped amount, split by the exchange's
    // configured share. Booking `close_fee_quote_unclamped` would have added
    // roughly seventy times this.
    let split = sakura_perps_risk::position::fee_split_quote(u128::from(gross), 1_000).unwrap();
    let (pending_after_close, deposited_after_close) = fee_revenue(&fixture);
    assert_eq!(
        u128::from(pending_after_close - pending_after_open),
        split.protocol_quote,
        "the protocol's share is taken from the retained fee, not the computed one"
    );
    assert_eq!(
        u128::from(deposited_after_close - deposited_after_open),
        u128::from(position.collateral_quote) - u128::from(gross) + split.lp_quote,
        "liquidity providers take the loss plus their share of the retained fee"
    );

    // The whole payout was consumed by the fee, so the vault kept every token
    // it held. Booking the *computed* fee would have credited the pool a
    // multiple of this against a vault that never moved.
    assert_eq!(
        fixture.token_balance(fixture.quote_vault),
        vault_after_open,
        "a fully-consumed payout must move no tokens either"
    );
    fixture.assert_i1("after the fee-clamped close");
    assert!(!fixture.position_exists());
}

/// **B1, the control.** A profitable close, where the clamp must *not* bind.
///
/// The two halves above both sit on the **binding** side of `min(ceil(fee),
/// gross_payout)`: one pins the output at zero, the other pins it at the whole
/// payout. Neither says anything about the other branch, so between them they
/// are satisfied by a `settle_close` that reads
/// `if ceil(fee) > gross { gross } else { 0 }` — the clamp applied faithfully
/// and the ordinary case forgotten. That is a real over-correction of B1 with
/// the mirror-image consequence: the pool forgoes revenue the vault genuinely
/// retained, `quote_deposited` drifts *below* the vault rather than above it,
/// and the surplus is silently redistributed to whoever withdraws next.
///
/// This is also the only one of the three where tokens actually leave the vault
/// — in both of the others the net payout is zero and the balance never moves —
/// so it is the only place the I1 equality is exercised across an outflow.
///
/// Equity is comfortably positive and the computed fee is a small fraction of
/// the payout, so `min` must return `ceil(fee)` untouched, the pool must book
/// exactly that split by the exchange's configured share, and the vault must
/// fall by precisely the net payout and by nothing else.
#[test]
fn a_profitable_close_books_the_full_fee_and_the_clamp_does_not_bind() {
    let mut fixture = Fixture::new(active_params());
    fixture.assert_i1("after the liquidity deposit");

    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open a $10,000 long against $1,100 of collateral");
    fixture.assert_i1("after the open");

    let (pending_after_open, deposited_after_open) = fee_revenue(&fixture);
    let vault_after_open = fixture.token_balance(fixture.quote_vault);
    let trader_after_open = fixture.token_balance(fixture.trader_token_account);
    let position = fixture.position_state();

    // Up 4%. The EMA moves with the spot, so the divergence gate is not what is
    // under examination, and the profit cap — $4,000, being 4,000 bps of entry
    // notional — is an order of magnitude away from binding.
    fixture.set_price(10_400);
    let expected = expected_close(&fixture, price_at(10_400));

    // Non-vacuity, and here it is the whole point: the clamp must have been
    // offered a chance to bind and must have declined it.
    assert!(
        expected.equity_usd > 0,
        "the control needs positive equity, not the zero branch: {}",
        expected.equity_usd
    );
    assert!(
        expected.close_fee_quote_unclamped > 0,
        "a zero fee would make the assertion below vacuous"
    );
    assert!(
        expected.close_fee_quote_unclamped < expected.gross_payout_quote,
        "the clamp must not bind here: fee {} vs gross {}",
        expected.close_fee_quote_unclamped,
        expected.gross_payout_quote
    );
    assert!(
        u128::from(position.collateral_quote) + u128::from(position.reserve_quote)
            > expected.gross_payout_quote,
        "the profit cap must not bind either, or the payout is not what equity says"
    );

    let logs = fixture.close().expect("a profitable close must succeed");
    let closed: PositionClosed = event(&logs);

    let gross = u64::try_from(expected.gross_payout_quote).unwrap();
    let fee = u64::try_from(expected.close_fee_quote_unclamped).unwrap();
    assert_eq!(closed.exit_price, expected.exit_price);
    assert_eq!(closed.gross_payout_quote, gross);
    assert_eq!(
        closed.close_fee_quote, fee,
        "an unclamped fee must be booked in full: this is the case the two \
         halves above cannot tell apart from a settle_close that books the \
         fee only when the clamp binds"
    );
    assert_eq!(
        closed.liquidation_fee_quote, 0,
        "an ordinary close is not a liquidation"
    );
    assert_eq!(closed.net_payout_quote, gross - fee);
    assert!(
        closed.net_payout_quote > 0,
        "a profitable close must pay the trader something"
    );
    assert_eq!(closed.bad_debt_usd, 0);
    assert!(!closed.profit_capped, "the profit cap must not have bound");
    assert_eq!(
        closed.close_fee_quote + closed.liquidation_fee_quote + closed.net_payout_quote,
        closed.gross_payout_quote,
        "the four amounts must re-sum"
    );

    // The vault fell by the net payout and by nothing else. The fee never left
    // it — which is what makes booking it as revenue honest *here*, and what
    // makes booking it in either of the two cases above a lie about a transfer
    // that did not happen.
    let vault_after_close = fixture.token_balance(fixture.quote_vault);
    assert_eq!(
        vault_after_open - vault_after_close,
        closed.net_payout_quote,
        "only the net payout may leave the vault"
    );
    assert_eq!(
        fixture.token_balance(fixture.trader_token_account) - trader_after_open,
        closed.net_payout_quote,
        "and it must land in the trader's own account"
    );

    // Fee revenue rose by the full computed fee, split by the exchange's share.
    let split = sakura_perps_risk::position::fee_split_quote(u128::from(fee), 1_000).unwrap();
    assert!(
        split.protocol_quote > 0 && split.lp_quote > 0,
        "both halves of the split must be non-zero for this to discriminate"
    );
    let (pending_after_close, deposited_after_close) = fee_revenue(&fixture);
    assert_eq!(
        u128::from(pending_after_close - pending_after_open),
        split.protocol_quote,
        "the protocol takes its share of the fee the vault actually retained"
    );
    assert_eq!(
        i128::from(deposited_after_close) - i128::from(deposited_after_open),
        i128::from(position.collateral_quote) - i128::from(gross) + split.lp_quote as i128,
        "liquidity providers fund the profit and keep their share of the fee"
    );

    fixture.assert_i1("after the profitable close");
    assert!(!fixture.position_exists(), "the position must be gone");
}

// ════════════════════════════════════════════════════════════════════════════
// B2 — the exit that must survive everything.
// ════════════════════════════════════════════════════════════════════════════

/// **B2.** A revoked feed, a quarantined market, every pause flag set, and the
/// pinned price account destroyed — and the position still closes.
///
/// The three ordinary exits are checked in the same state first, because a test
/// that only showed `emergency_close_position` working would prove nothing about
/// why it exists. `close_position` and `admin_settle_position` are refused here
/// with a live, fresh price account in place, so the pause is demonstrably the
/// only thing stopping them.
#[test]
fn the_emergency_exit_survives_revocation_quarantine_and_a_full_pause() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");
    fixture.assert_i1("after the open");

    let position = fixture.position_state();
    let trader_before = fixture.token_balance(fixture.trader_token_account);

    // 1. The admin stops trusting the feed. Opening reads this; nothing else
    //    does.
    fixture.set_feed_revoked(true);
    // 2. And closes the market to new risk, which starts the wind-down clock.
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    let quarantined_ts = fixture.market_state().quarantined_ts;
    assert_eq!(quarantined_ts, fixture.now_unix);

    // 3. A day passes, and the market moves while it does. The price account is
    //    rewritten at the new clock so it is fresh, which is what makes the two
    //    refusals below attributable to the pause alone rather than to
    //    staleness.
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);
    fixture.set_price(10_500);

    // 4. The permissionless reference advances while everything else is down.
    //    This is what stops an admin pausing both trading paths, waiting for the
    //    market to move, and emergency-closing at the stale reference the last
    //    open left behind.
    //
    //    The new price is deliberately *different* from the one the open wrote:
    //    asserting the reference against an unchanged level would hold just as
    //    well if the instruction did nothing at all.
    let stale = fixture.market_state().last_good_price;
    assert_eq!(
        stale,
        price_at(10_000),
        "the open left the reference at the price it traded at"
    );
    fixture.set_pause_flags(PauseFlags::ALL);
    fixture
        .refresh_market_price()
        .expect("refresh_market_price is neither pausable nor admin-gated");
    let market = fixture.market_state();
    assert_eq!(
        market.last_good_price,
        price_at(10_500),
        "the reference must track the oracle while both trading paths are shut"
    );
    assert_ne!(market.last_good_price, stale);
    assert_eq!(market.last_good_price_ts, fixture.now_unix);

    // 5. Both ordinary exits are shut, with a valid price account present.
    expect_error(fixture.close().map(|_| ()), PerpsError::ClosingPaused);
    expect_error(
        fixture.admin_settle().map(|_| ()),
        PerpsError::LiquidationPaused,
    );

    // 6. Destroy the oracle account outright. Nothing survives this except an
    //    instruction that never asks for it — which is the whole design.
    fixture
        .svm
        .set_account(fixture.price_update, Account::default())
        .expect("delete the price account");
    assert!(
        fixture.refresh_market_price().is_err(),
        "with no price account even the permissionless refresh must fail, or \
         the next assertion proves nothing"
    );

    let logs = fixture
        .emergency_close()
        .expect("emergency close must survive all of it");
    let closed: PositionClosed = event(&logs);

    // Settled off `last_good_price`, with no confidence — there was no oracle
    // to read one from — and the position's own snapshotted spread.
    let expected_exit = execution_price(
        Side::Long,
        PriceDirection::Close,
        market.last_good_price,
        0,
        position.spread_bps,
    )
    .unwrap();
    assert_eq!(
        closed.exit_price, expected_exit,
        "the emergency exit settles from market.last_good_price"
    );
    assert_eq!(
        closed.close_fee_quote + closed.liquidation_fee_quote + closed.net_payout_quote,
        closed.gross_payout_quote
    );
    assert_eq!(
        closed.liquidation_fee_quote, 0,
        "a wind-down is not a liquidation and takes no liquidation fee"
    );

    // The collateral genuinely came back.
    let recovered = fixture.token_balance(fixture.trader_token_account) - trader_before;
    assert_eq!(recovered, closed.net_payout_quote);
    assert!(
        recovered > 0,
        "the point of the instruction is that value is recovered"
    );
    assert!(!fixture.position_exists());
    fixture.assert_i1("after the emergency close");

    // And the market is empty, at entry notional.
    let market = fixture.market_state();
    assert_eq!((market.long_oi_usd, market.long_positions), (0, 0));
    assert_eq!((market.locked_quote, market.reserved_quote), (0, 0));
}

/// **B2, the half that makes the emergency exit safe rather than merely
/// available.** `refresh_market_price` requires no signer at all.
///
/// The emergency exit settles from `market.last_good_price`. The only
/// instructions that write that field in the ordinary course — `open_position`,
/// `close_position` and `admin_settle_position` — are every one of them
/// pausable by the admin. Were that the whole story, an admin could pause all
/// three, wait for the market to move, and emergency-close every position at
/// the reference the last trade happened to leave behind. This instruction is
/// what removes that: anyone at all can advance the reference for the cost of
/// one transaction, so freezing it requires the feed itself to be dead — in
/// which case `last_good_price` genuinely is the last honest price there was.
///
/// "Anyone at all" is proven twice over: structurally, in that the instruction's
/// account list contains no signer for the program to check, and then in
/// practice, by sending it from a keypair holding no position, no pool shares
/// and no authority, into a market that is revoked, quarantined and fully
/// paused.
#[test]
fn refresh_market_price_is_permissionless_and_advances_the_reference() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    // Structural. Two accounts, neither a signer: there is no authority the
    // instruction could be checking, because none is presented to it.
    let instruction = fixture.refresh_market_price_ix();
    assert_eq!(
        instruction.accounts.len(),
        2,
        "the market and its pinned price account, and nothing else: {:?}",
        instruction.accounts
    );
    assert!(
        instruction.accounts.iter().all(|meta| !meta.is_signer),
        "refresh_market_price must require no signer: {:?}",
        instruction.accounts
    );

    // The most hostile state the admin can put the exchange into.
    fixture.set_feed_revoked(true);
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    fixture.set_pause_flags(PauseFlags::ALL);

    let stale = fixture.market_state().last_good_price;
    assert_eq!(stale, price_at(10_000), "the open set the reference");
    let book_before = {
        let market = fixture.market_state();
        (
            market.long_oi_usd,
            market.locked_quote,
            market.reserved_quote,
        )
    };
    let vault_before = fixture.token_balance(fixture.quote_vault);

    // Time passes and the market moves away from the last trade.
    fixture.advance(600, 10);
    fixture.set_price(11_750);

    // A stranger: enough lamports to pay the fee, and no other relationship to
    // the exchange whatsoever.
    let stranger = Keypair::new();
    fixture
        .svm
        .airdrop(&stranger.pubkey(), 1_000_000_000)
        .unwrap();
    fixture
        .send(instruction, &[&stranger])
        .expect("a stranger must be able to advance the emergency reference");

    let market = fixture.market_state();
    assert_eq!(
        market.last_good_price,
        price_at(11_750),
        "the reference must track the oracle, not the last trade"
    );
    assert_ne!(market.last_good_price, stale);
    assert_eq!(market.last_good_price_ts, fixture.now_unix);

    // And it moved nothing else. This instruction touches no value, which is
    // why it can afford to take no signer.
    assert_eq!(
        (
            market.long_oi_usd,
            market.locked_quote,
            market.reserved_quote
        ),
        book_before,
        "a permissionless instruction must not be able to move the book"
    );
    assert_eq!(fixture.token_balance(fixture.quote_vault), vault_before);
    assert!(
        book_before.1 > 0,
        "the book has to be non-empty to be worth guarding"
    );
    fixture.assert_i1("after a permissionless refresh");
}

/// **B2, the fallback.** With `last_good_price` at zero the emergency exit
/// settles from the position's own `entry_price`.
///
/// `emergency_reference_price` reads `market.last_good_price` when it is
/// non-zero and `position.entry_price` otherwise. The fallback is for a market
/// listed, traded into and re-quarantined by a build that predates the field,
/// where the reference genuinely reads zero. `entry_price` is not a market
/// price, but it is a price this exact position transacted at — and the
/// alternative is handing `execution_price` a zero mid, which returns
/// `OracleInvalidPrice`. The one instruction that exists so a position can
/// always get out would become the one instruction that cannot run, in exactly
/// the state where it is the only exit left.
///
/// So this test fails as an **`Err`** if the fallback is removed, not as a wrong
/// number.
///
/// No instruction clears `last_good_price`, so the state is reached by writing
/// the market account directly — see `Fixture::write_market`.
#[test]
fn the_emergency_exit_falls_back_to_the_positions_entry_price() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");
    let position = fixture.position_state();
    let trader_before = fixture.token_balance(fixture.trader_token_account);

    // Non-vacuity: the two candidate references have to differ, or the assertion
    // below cannot tell which one was used. The open wrote the unclamped mid;
    // the entry price is that mid moved by the spread.
    let reference_before = fixture.market_state().last_good_price;
    assert_eq!(reference_before, price_at(10_000));
    assert_ne!(
        position.entry_price, reference_before,
        "the spread must separate the two references or this proves nothing"
    );

    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);

    // The legacy state: a market carrying positions and no recorded reference.
    fixture.write_market(|market| {
        market.last_good_price = 0;
        market.last_good_price_ts = 0;
    });
    let market = fixture.market_state();
    assert_eq!(market.last_good_price, 0);
    assert_eq!(
        market.quarantined_ts, NOW_UNIX,
        "the rewrite must have left the rest of the market alone"
    );

    // And no oracle to recover one from. `refresh_market_price` is the way back
    // out of this state, so destroying the price account is what makes the
    // fallback the only thing left standing.
    fixture
        .svm
        .set_account(fixture.price_update, Account::default())
        .expect("delete the price account");
    assert!(
        fixture.refresh_market_price().is_err(),
        "with no price account the reference cannot be restored, which is the \
         premise of everything below"
    );

    let logs = fixture
        .emergency_close()
        .expect("a zero reference must fall back, not fail");
    let closed: PositionClosed = event(&logs);

    let expected_exit = execution_price(
        Side::Long,
        PriceDirection::Close,
        position.entry_price,
        0,
        position.spread_bps,
    )
    .unwrap();
    assert_eq!(
        closed.exit_price, expected_exit,
        "the fallback reference is the position's own entry price"
    );
    assert!(
        closed.exit_price < position.entry_price,
        "closing a long still sells at the bid: {} vs entry {}",
        closed.exit_price,
        position.entry_price
    );
    assert_eq!(
        closed.liquidation_fee_quote, 0,
        "a wind-down is not a liquidation and takes no liquidation fee"
    );
    assert_eq!(
        closed.close_fee_quote + closed.liquidation_fee_quote + closed.net_payout_quote,
        closed.gross_payout_quote
    );

    let recovered = fixture.token_balance(fixture.trader_token_account) - trader_before;
    assert_eq!(recovered, closed.net_payout_quote);
    assert!(
        recovered > 0,
        "the collateral has to come back; that is the entire point"
    );
    assert!(!fixture.position_exists());
    fixture.assert_i1("after an emergency close off the entry price");

    let market = fixture.market_state();
    assert_eq!((market.long_oi_usd, market.long_positions), (0, 0));
    assert_eq!((market.locked_quote, market.reserved_quote), (0, 0));
}

/// The wind-down delay runs from the quarantine, and a retune that does not
/// cross the quarantine boundary must not restart it.
///
/// `quarantined_ts` is written **only on the transition**. Writing it on every
/// call would mean an admin tidying up fees silently postponed every trapped
/// position's only remaining exit by another day, repeatedly, without ever
/// taking an action that looks like a refusal.
#[test]
fn a_retune_inside_quarantine_does_not_restart_the_wind_down_delay() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    let quarantined_ts = fixture.market_state().quarantined_ts;

    // One second short of the delay: refused, and that refusal is what makes the
    // success below meaningful.
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS - 1, 10);
    expect_error(
        fixture.emergency_close().map(|_| ()),
        PerpsError::EmergencyCloseTooSoon,
    );

    // A retune that stays inside the quarantine. If this restarted the clock,
    // the close below would be refused for another whole day.
    fixture
        .set_risk_params(
            fixture.market,
            RiskParams {
                close_fee_bps: 20,
                max_oracle_drift_bps: 20,
                ..quarantined_params(active_params())
            },
        )
        .expect("retune while quarantined");
    assert_eq!(
        fixture.market_state().quarantined_ts,
        quarantined_ts,
        "a retune that does not cross the boundary must leave quarantined_ts alone"
    );

    fixture.advance(2, 1);
    fixture
        .emergency_close()
        .expect("the delay is measured from the original quarantine");
    fixture.assert_i1("after the emergency close");
}

/// Tightening must never trap. Quarantine and revocation close the market to new
/// risk and gate **nothing** on the way out.
#[test]
fn a_quarantined_market_with_a_revoked_feed_still_lets_a_position_out() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    fixture.set_feed_revoked(true);
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");

    // Opening is shut. A newcomer, so the refusal is the revocation talking and
    // not the fixture trader's existing position PDA colliding — see
    // `Fixture::newcomer`.
    let (newcomer, newcomer_tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        newcomer_tokens,
        SIDE_SHORT,
        ONE_UNIT,
        100 * ONE,
        unbounded_limit(SIDE_SHORT),
    );
    expect_error(
        fixture.send(instruction, &[&newcomer]),
        PerpsError::FeedRevoked,
    );

    // Closing is not.
    fixture
        .close()
        .expect("an ordinary close is gated by the pause flag and nothing else");
    fixture.assert_i1("after closing out of a quarantined market");
}

// ════════════════════════════════════════════════════════════════════════════
// B3 — the liquidation fee is clamped twice.
// ════════════════════════════════════════════════════════════════════════════

/// **B3.** The specification's late-liquidation case, end to end.
///
/// $10,000 of notional against $500 of collateral at 100 bps, with equity
/// decayed to about $50. `liquidation_fee` caps the fee against the
/// **collateral**, which does nothing here — $95 of fee against $500 of
/// collateral passes that clamp untouched. The second clamp, against what the
/// close fee left of the gross payout, is the one that matters: without it the
/// subtraction underflows, `admin_settle_position` returns `MathOverflow`, and
/// the position is unliquidatable on the only liquidation path M5 ships.
///
/// # Why only the second clamp is proven here, and why that is not a gap
///
/// The first clamp — `liquidation_fee`'s own `fee.min(collateral_remaining_usd)`
/// — has **no reachable case** in which it is the binding one and the binding
/// is observable. For it to bind at all,
/// `liquidation_fee_bps × current_notional > collateral`, and validation 1 on
/// `set_risk_params` forces `initial_margin_bps > liquidation_fee_bps`, so
/// current notional must have grown well past entry notional. That only happens
/// on a long whose price rose or a short whose price fell — the profitable
/// direction — and a position that far in profit is not liquidatable, so
/// `admin_settle_position` refuses it before the fee is computed.
/// `emergency_close_position`, the only other caller, passes a liquidation fee
/// of zero. Where the fee *is* clamped in a reachable state, gross payout is
/// already zero and the second clamp returns the same answer, so the two are
/// indistinguishable from outside the instruction.
///
/// It is not unguarded: `crates/risk/tests/invariants.rs`'s
/// `liquidation_fee_never_exceeds_collateral` is a property test over the
/// function directly, which is the right level for a clamp with no reachable
/// instruction-level witness. Deleting it leaves this suite green, and that is
/// a statement about reachability rather than about coverage.
#[test]
fn a_late_liquidation_clamps_the_fee_against_the_payout_and_still_closes() {
    // 5% initial margin, so $500 of collateral supports exactly $10,000 of
    // notional. The spread is set to zero here — with `max_oracle_drift_bps`
    // lowered to the 20 bps the two fees still dominate, which is validation 9 —
    // so the specification's figures land exactly rather than to the nearest
    // basis point: entry at $100.00, notional $10,000.00, collateral $500.00.
    // Every other test in this file carries a live spread; what this one is
    // about is the second clamp, and an exact scenario makes each of the four
    // payout fields a number that can be stated rather than bounded.
    let mut fixture = Fixture::new(RiskParams {
        initial_margin_bps: 500,
        maintenance_margin_bps: 200,
        liquidation_fee_bps: 100,
        max_profit_bps: 2_000,
        spread_bps: 0,
        max_oracle_drift_bps: 20,
        ..active_params()
    });

    // $510 in, of which $10 is the opening fee at 10 bps of $10,000.
    fixture
        .open(SIDE_LONG, BIG_SIZE, 510 * ONE)
        .expect("open a $10,000 long on $500 of collateral");
    fixture.assert_i1("after the open");
    let position = fixture.position_state();
    let trader_before = fixture.token_balance(fixture.trader_token_account);

    // The specification's case, as the program itself recorded it.
    assert_eq!(position.entry_price, price_at(10_000));
    assert_eq!(position.entry_notional_usd, 10_000 * USD_SCALE);
    assert_eq!(
        position.collateral_quote,
        500 * ONE,
        "the fee must leave exactly $500 of collateral"
    );
    assert_eq!(position.liquidation_fee_bps, 100);

    // Down 4.5%: equity decays to exactly $50 — a $450 loss against $500 of
    // collateral — while the liquidation fee is still computed on $9,550 of
    // current notional, which is $95.50, nearly twice what is left.
    fixture.set_price(9_550);
    let expected = expected_close(&fixture, price_at(9_550));
    assert_eq!(
        expected.equity_usd,
        i128::try_from(50 * USD_SCALE).unwrap(),
        "the specification's decayed equity"
    );
    assert_eq!(expected.current_notional_usd, 9_550 * USD_SCALE);

    // Non-vacuity 1: the position is liquidatable, judged at current notional.
    assert!(
        is_liquidatable(
            expected.equity_usd,
            expected.current_notional_usd,
            position.maintenance_margin_bps
        )
        .unwrap(),
        "the position has to be liquidatable or the instruction never runs"
    );
    // Non-vacuity 2: the collateral clamp does **not** bind, so only the second
    // clamp can be what saves this.
    let collateral_usd =
        quote_to_usd_floor(u128::from(position.collateral_quote), COLLATERAL_DECIMALS).unwrap();
    let liq_fee_usd = liquidation_fee(
        expected.current_notional_usd,
        position.liquidation_fee_bps,
        collateral_usd,
    )
    .unwrap();
    assert_eq!(
        (liq_fee_usd, collateral_usd),
        (9_550 * USD_SCALE / 100, 500 * USD_SCALE),
        "100 bps of $9,550 is $95.50, against $500 of collateral"
    );
    assert!(
        liq_fee_usd < collateral_usd,
        "liquidation_fee's own cap must not be the clamp under test"
    );
    // Non-vacuity 3: the fee genuinely exceeds what the close fee left.
    let close_fee_quote = expected
        .close_fee_quote_unclamped
        .min(expected.gross_payout_quote);
    let after_close_fee = expected.gross_payout_quote - close_fee_quote;
    assert!(
        usd_to_quote_ceil(liq_fee_usd, COLLATERAL_DECIMALS).unwrap() > after_close_fee,
        "the payout clamp must bind: fee {} vs {} left after the close fee",
        usd_to_quote_ceil(liq_fee_usd, COLLATERAL_DECIMALS).unwrap(),
        after_close_fee
    );

    let logs = fixture
        .admin_settle()
        .expect("a late liquidation must not underflow");
    let closed: PositionClosed = event(&logs);

    assert_eq!(
        closed.gross_payout_quote,
        u64::try_from(expected.gross_payout_quote).unwrap()
    );
    assert_eq!(
        closed.close_fee_quote,
        u64::try_from(close_fee_quote).unwrap(),
        "the close fee is taken first, always"
    );
    assert_eq!(
        closed.liquidation_fee_quote,
        u64::try_from(after_close_fee).unwrap(),
        "the liquidation fee takes only what the close fee left"
    );
    assert_eq!(
        closed.close_fee_quote + closed.liquidation_fee_quote + closed.net_payout_quote,
        closed.gross_payout_quote,
        "the four payout fields must re-sum"
    );

    // The same four numbers stated outright, so a reader can check the case
    // against the specification without re-deriving `expected_close`: $50 of
    // gross, $9.55 of close fee at 10 bps of $9,550, and a liquidation fee that
    // wanted $95.50 and is handed the $40.45 that was left.
    assert_eq!(
        (
            closed.gross_payout_quote,
            closed.close_fee_quote,
            closed.liquidation_fee_quote,
            closed.net_payout_quote,
        ),
        (50 * ONE, 9_550_000, 40_450_000, 0),
    );
    // `net_payout_quote >= 0` is a type-level fact on a `u64`. The substantive
    // claim is that it is exactly zero and the position still closed: at this
    // depth the two fees consume the whole of the remaining equity, and a
    // trader who is liquidated this late correctly receives nothing. What must
    // not happen — and is what the missing clamp causes — is the subtraction
    // going below zero and reverting the only liquidation path M5 ships.
    assert_eq!(
        fixture.token_balance(fixture.trader_token_account) - trader_before,
        closed.net_payout_quote,
        "no token may move when the payout is zero"
    );

    assert!(!fixture.position_exists(), "the position closes");
    fixture.assert_i1("after the late liquidation");

    let market = fixture.market_state();
    assert_eq!((market.long_oi_usd, market.long_positions), (0, 0));
    assert_eq!((market.locked_quote, market.reserved_quote), (0, 0));
}

// ════════════════════════════════════════════════════════════════════════════
// B4 — account substitution.
// ════════════════════════════════════════════════════════════════════════════

/// Give the fixture a second qualified feed and a second listed market.
fn list_second_market(fixture: &mut Fixture) -> Address {
    let price_update = fixture.other_price_update(OTHER_FEED_ID);
    fixture.write_price(price_update, OTHER_FEED_ID, 10_000);
    fixture
        .qualify_feed(OTHER_FEED_ID)
        .expect("qualify the second feed");
    fixture
        .create_market(OTHER_FEED_ID)
        .expect("list the second market");
    let market = pda(&[b"market", OTHER_FEED_ID.as_ref()]);
    fixture
        .set_risk_params(market, active_params())
        .expect("activate the second market");
    market
}

/// **B4.** A position opened in market A cannot be closed against market B.
///
/// Two attempts, because they fail on different constraints and both matter.
/// The naive substitution — market B with market A's position account — is
/// caught by the seeds. The crafted one puts market A's position bytes at the
/// address market B's seeds derive, so the seeds pass and only `has_one =
/// market` is left standing. That is the constraint whose absence let a position
/// be settled at the wrong price, on the wrong indices, against the wrong slice.
#[test]
fn a_position_cannot_be_closed_against_another_market() {
    let mut fixture = Fixture::new(active_params());
    let other_market = list_second_market(&mut fixture);
    let other_price_update = fixture.other_price_update(OTHER_FEED_ID);

    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open in market A");

    // Attempt 1: market B, with the position account market A really owns.
    let mut instruction = fixture.close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.market {
            meta.pubkey = other_market;
        } else if meta.pubkey == fixture.price_update {
            meta.pubkey = other_price_update;
        }
    }
    let trader = fixture.trader.insecure_clone();
    assert!(
        fixture.send(instruction, &[&trader]).is_err(),
        "the position seeds name the market; a mismatch must be refused"
    );

    // Attempt 2: the same bytes, written where market B's seeds point, with the
    // stored bump corrected so the seeds constraint is satisfied and `has_one`
    // is the only thing left to catch it.
    let owner = fixture.trader.pubkey();
    let (impostor, bump) = Address::find_program_address(
        &[b"position", other_market.as_ref(), owner.as_ref()],
        &sakura_perps::ID,
    );
    let mut account = fixture
        .svm
        .get_account(&fixture.position_key())
        .expect("position exists");
    // Layout: 8-byte discriminator, then `bump: u8`.
    account.data[8] = bump;
    fixture
        .svm
        .set_account(impostor, account)
        .expect("plant the impostor");

    let mut instruction = fixture.close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.market {
            meta.pubkey = other_market;
        } else if meta.pubkey == fixture.price_update {
            meta.pubkey = other_price_update;
        } else if meta.pubkey == fixture.position_key() {
            meta.pubkey = impostor;
        }
    }
    expect_error(
        fixture.send(instruction, &[&trader]),
        PerpsError::WrongMarket,
    );

    // And the real position is untouched by either attempt.
    assert!(fixture.position_exists());
    fixture.assert_i1("after two refused substitutions");
}

/// **B4.** A token account that is not the pool's vault cannot stand in for it.
#[test]
fn a_foreign_quote_vault_is_refused() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    // A real, funded, correctly-minted token account — everything except the
    // one thing that matters, which is being the PDA the pool signs for.
    let attacker = Keypair::new();
    fixture
        .svm
        .airdrop(&attacker.pubkey(), 10_000_000_000)
        .unwrap();
    let decoy =
        CreateAssociatedTokenAccount::new(&mut fixture.svm, &attacker, &fixture.collateral_mint)
            .owner(&attacker.pubkey())
            .send()
            .expect("decoy vault");
    MintTo::new(
        &mut fixture.svm,
        &fixture.admin.insecure_clone(),
        &fixture.collateral_mint,
        &decoy,
        100_000 * ONE,
    )
    .send()
    .expect("fund the decoy");

    let vault_before = fixture.token_balance(fixture.quote_vault);
    let decoy_before = fixture.token_balance(decoy);

    let mut instruction = fixture.close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.quote_vault {
            meta.pubkey = decoy;
        }
    }
    let trader = fixture.trader.insecure_clone();
    assert!(
        fixture.send(instruction, &[&trader]).is_err(),
        "a foreign quote vault must be refused"
    );

    assert_eq!(fixture.token_balance(fixture.quote_vault), vault_before);
    assert_eq!(fixture.token_balance(decoy), decoy_before);
    assert!(fixture.position_exists());

    // The open leg, where the money moves *in* and the substitution is worth
    // more. `open_position` measures the deposit as the vault's own balance
    // delta across the transfer, so a vault the caller controls would have the
    // pool credit collateral that never left the caller's pocket — I1 breaks on
    // the next close, and the position that breaks it is not the attacker's.
    // A newcomer opens it: an `init` position PDA that already exists fails on
    // `AccountAlreadyInUse` before the constraint under test is reached.
    let (newcomer, newcomer_tokens) = fixture.newcomer();
    let newcomer_before = fixture.token_balance(newcomer_tokens);

    let honest = fixture.open_ix_for(
        newcomer.pubkey(),
        newcomer_tokens,
        SIDE_LONG,
        ONE_UNIT,
        100 * ONE,
        unbounded_limit(SIDE_LONG),
    );
    let mut instruction = honest.clone();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.quote_vault {
            meta.pubkey = decoy;
        }
    }
    // `ConstraintSeeds` (2006), not a `PerpsError`: the vault is pinned by
    // `seeds = [b"quote_vault"], bump = pool.vault_bump`, so Anchor's own check
    // is what refuses this and `expect_error` could never match it. Asserted as
    // the exact observed code rather than `is_err`, because an open can fail for
    // a dozen unrelated reasons and a bare `is_err` cannot tell them apart.
    assert_eq!(
        fixture.send(instruction, &[&newcomer]),
        Err(TransactionError::InstructionError(
            0,
            InstructionError::Custom(2006)
        )),
        "a foreign quote vault must be refused on the open leg too"
    );

    assert_eq!(
        fixture.token_balance(newcomer_tokens),
        newcomer_before,
        "no collateral may have left the caller"
    );
    assert_eq!(
        fixture.token_balance(decoy),
        decoy_before,
        "and none may have reached the decoy"
    );
    assert_eq!(fixture.token_balance(fixture.quote_vault), vault_before);

    // The control, and the reason the rejection above means anything: the
    // byte-identical instruction with the pool's own vault succeeds. Without it
    // the refusal could be any of the other things that make an open fail, and
    // the substitution would be proving nothing about the vault at all.
    fixture
        .send(honest, &[&newcomer])
        .expect("the same open against the pool's own vault must succeed");
    assert_eq!(
        fixture.token_balance(newcomer_tokens),
        newcomer_before - 100 * ONE,
        "and that one really did move the collateral"
    );
    fixture.assert_i1("after the refused substitution and the honest open");
}

/// **B4.** On both admin-driven paths the payout destination is pinned to the
/// **position's** owner, not to whoever the admin nominated.
///
/// Without this an admin names their own token account and a liquidation becomes
/// a transfer to the liquidator, with the trader's rent as the only thing they
/// get back — and nothing else in the account list would notice.
#[test]
fn an_admin_cannot_redirect_a_payout_to_their_own_token_account() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    let admin_token_account = fixture.admin_token_account;
    let admin_before = fixture.token_balance(admin_token_account);

    // Liquidation. The constraint is on the account, so it fires before the
    // handler's liquidatability gate is ever reached.
    let mut instruction = fixture.admin_settle_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.trader_token_account {
            meta.pubkey = admin_token_account;
        }
    }
    let admin = fixture.admin.insecure_clone();
    expect_error(
        fixture.send(instruction, &[&admin]),
        PerpsError::NotTokenOwner,
    );

    // The emergency path, which needs the market quarantined before the
    // constraint on the token account is reached at all.
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);

    let mut instruction = fixture.emergency_close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.trader_token_account {
            meta.pubkey = admin_token_account;
        }
    }
    expect_error(
        fixture.send(instruction, &[&admin]),
        PerpsError::NotTokenOwner,
    );

    assert_eq!(fixture.token_balance(admin_token_account), admin_before);
    assert!(fixture.position_exists());
}

/// **B4.** A price update account that is not the one the market was pinned to
/// is refused, even when it is a genuine, fresh, correctly-signed update for the
/// very same feed id.
///
/// The feed-id check inside the Pyth SDK proves the *message* is for the right
/// feed. It does not prove the *account* was written by anyone trustworthy, and
/// that gap is what `address = market.price_update` closes.
#[test]
fn a_price_update_that_is_not_the_markets_own_is_refused() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    // Same feed id, same exponent, fresh on both clocks — and a different
    // account, which is the only difference that matters.
    let doppelganger = Address::new_unique();
    fixture.write_price(doppelganger, FEED_ID, 9_000);

    let mut instruction = fixture.close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.price_update {
            meta.pubkey = doppelganger;
        }
    }
    let trader = fixture.trader.insecure_clone();
    expect_error(
        fixture.send(instruction, &[&trader]),
        PerpsError::WrongPriceUpdate,
    );

    // The same substitution on the open leg, so the constraint is shown on both
    // of the instructions that take the account. A newcomer opens it, per
    // `Fixture::newcomer`.
    let (newcomer, newcomer_tokens) = fixture.newcomer();
    let mut instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        newcomer_tokens,
        SIDE_SHORT,
        ONE_UNIT,
        100 * ONE,
        unbounded_limit(SIDE_SHORT),
    );
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.price_update {
            meta.pubkey = doppelganger;
        }
    }
    expect_error(
        fixture.send(instruction, &[&newcomer]),
        PerpsError::WrongPriceUpdate,
    );

    // And the liquidation leg, which completes the set of three instructions
    // that take the account. The constraint is on the account, so it fires
    // before the handler's liquidatability gate — a healthy position is refused
    // here for the oracle, not for being healthy.
    let mut instruction = fixture.admin_settle_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.price_update {
            meta.pubkey = doppelganger;
        }
    }
    let admin = fixture.admin.insecure_clone();
    expect_error(
        fixture.send(instruction, &[&admin]),
        PerpsError::WrongPriceUpdate,
    );

    assert!(fixture.position_exists());
}

/// **B4.** The permissionless writer is the one substitution with no authority
/// in front of it, so `address = market.price_update` is all that stands between
/// a stranger and the emergency settlement reference.
///
/// `refresh_market_price` takes no signer at all. That is safe *only* because
/// the account it reads is pinned to the one the market was qualified against.
/// Were it not, anyone could hand it a fabricated `PriceUpdateV2` — this test
/// writes a convincing one in two lines — and set `last_good_price` to any
/// number they liked, for the price of one transaction, then wait out the
/// quarantine delay and drain the market through the emergency exit. Every
/// other substitution in this file needs the position's owner or the admin;
/// this one needs nobody, which is what makes it the worst of them.
#[test]
fn a_fabricated_price_update_cannot_poison_the_permissionless_refresh() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    let honest = fixture.market_state().last_good_price;
    assert_eq!(honest, price_at(10_000), "the open set the reference");

    // A forgery with nothing wrong with its contents: right feed id, right
    // exponent, fresh on both clocks, owned by the real Pyth receiver and
    // verified Full — and at five times the price. Every check the program
    // could make about what it *says* passes.
    let forgery = Address::new_unique();
    fixture.write_price(forgery, FEED_ID, 50_000);

    let mut instruction = fixture.refresh_market_price_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.price_update {
            meta.pubkey = forgery;
        }
    }
    // Sent by a stranger holding no position, no shares and no authority,
    // because that is exactly who would be sending it.
    let stranger = Keypair::new();
    fixture
        .svm
        .airdrop(&stranger.pubkey(), 1_000_000_000)
        .unwrap();
    expect_error(
        fixture.send(instruction, &[&stranger]),
        PerpsError::WrongPriceUpdate,
    );

    assert_eq!(
        fixture.market_state().last_good_price,
        honest,
        "a refused refresh must leave the reference exactly where it was"
    );

    // What the forgery was for. The emergency exit consults no oracle: it
    // settles off `last_good_price`, so poisoning that field is poisoning every
    // emergency close in the market at once.
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);

    let position = fixture.position_state();
    let logs = fixture
        .emergency_close()
        .expect("the emergency exit still works");
    let closed: PositionClosed = event(&logs);
    assert_eq!(
        closed.exit_price,
        execution_price(
            Side::Long,
            PriceDirection::Close,
            honest,
            0,
            position.spread_bps
        )
        .unwrap(),
        "the exit must be struck off the honest reference, not the forgery"
    );
    assert!(
        closed.exit_price < price_at(50_000),
        "and nowhere near the forged price"
    );
    fixture.assert_i1("after the emergency close");
}

/// **B4.** The emergency exit is the one close whose settlement price comes from
/// the *market account it is handed*, which is what makes `has_one = market`
/// matter more here than anywhere else.
///
/// Every other close reads its price from an oracle the market pins by
/// `address = market.price_update`, so substituting the market alone changes
/// nothing about the price. Emergency close reads no oracle: it reads
/// `market.last_good_price` straight off the account in the meta list.
/// Substituting the market therefore substitutes the price directly, and needs
/// no forgery at all — any other quarantined market in the exchange will do, and
/// the admin picks whichever one is furthest in their favour.
#[test]
fn the_emergency_exit_cannot_be_settled_against_another_markets_price() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open a long in market A at $100.00");

    let other_market = list_second_market(&mut fixture);
    let other_price_update = fixture.other_price_update(OTHER_FEED_ID);

    // Both markets quarantined, so the impostor satisfies the emergency path's
    // own `is_quarantined` constraint and cannot be what refuses the attempt.
    // The delay runs from each market's own `quarantined_ts`.
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine market A");
    fixture
        .set_risk_params(other_market, quarantined_params(active_params()))
        .expect("quarantine market B");
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);

    // Market B's reference is driven to five times market A's, which is the
    // whole motive: a long settled against it is paid for a move that never
    // happened in the market it was actually opened in.
    fixture.write_price(other_price_update, OTHER_FEED_ID, 50_000);
    fixture
        .refresh_market_price_for(other_market, other_price_update)
        .expect("advance market B's reference");
    assert_eq!(
        fixture.market_state_at(other_market).last_good_price,
        price_at(50_000)
    );
    assert_eq!(
        fixture.market_state().last_good_price,
        price_at(10_000),
        "market A's reference is unmoved — the two must differ or the \
         substitution would be worth nothing and prove nothing"
    );

    // Attempt 1: market B, with the position account market A really owns. The
    // position's seeds name the market, so the seeds catch it.
    let mut instruction = fixture.emergency_close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.market {
            meta.pubkey = other_market;
        }
    }
    let admin = fixture.admin.insecure_clone();
    assert!(
        fixture.send(instruction, &[&admin]).is_err(),
        "the position seeds name the market; a mismatch must be refused"
    );

    // Attempt 2: the same bytes planted where market B's seeds point, with the
    // stored bump corrected, so the seeds are satisfied and `has_one = market`
    // is the only thing left standing.
    let owner = fixture.trader.pubkey();
    let (impostor, bump) = Address::find_program_address(
        &[b"position", other_market.as_ref(), owner.as_ref()],
        &sakura_perps::ID,
    );
    let mut account = fixture
        .svm
        .get_account(&fixture.position_key())
        .expect("position exists");
    // Layout: 8-byte discriminator, then `bump: u8`.
    account.data[8] = bump;
    fixture
        .svm
        .set_account(impostor, account)
        .expect("plant the impostor");

    let mut instruction = fixture.emergency_close_ix();
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.market {
            meta.pubkey = other_market;
        } else if meta.pubkey == fixture.position_key() {
            meta.pubkey = impostor;
        }
    }
    expect_error(
        fixture.send(instruction, &[&admin]),
        PerpsError::WrongMarket,
    );

    // The honest exit still works, and settles off market A's own reference —
    // so the refusals above are the constraint doing its job, not the emergency
    // path being broken by the setup.
    let position = fixture.position_state();
    let logs = fixture
        .emergency_close()
        .expect("the honest emergency exit works");
    let closed: PositionClosed = event(&logs);
    assert_eq!(
        closed.exit_price,
        execution_price(
            Side::Long,
            PriceDirection::Close,
            price_at(10_000),
            0,
            position.spread_bps
        )
        .unwrap(),
        "settled against market A, at market A's reference"
    );
    fixture.assert_i1("after the honest emergency close");
}

// ════════════════════════════════════════════════════════════════════════════
// The rest: invariants, counters, snapshots, and the clock.
// ════════════════════════════════════════════════════════════════════════════

/// I1 balances to the unit across a full round trip, and every counter returns
/// to exactly zero.
///
/// Open interest comes off at **entry** notional, which is what makes the second
/// half true. Subtracting exit notional would leave a residue proportional to
/// how far the price moved, and I4 — a side has open interest if and only if it
/// has positions — is the assertion that catches it having done so.
#[test]
fn a_round_trip_balances_the_vault_and_empties_the_book() {
    for (side, exit_cents) in [(SIDE_LONG, 10_400u64), (SIDE_SHORT, 9_600u64)] {
        let mut fixture = Fixture::new(active_params());
        let deposited_before = fixture.pool_state().quote_deposited;
        fixture.assert_i1("before the open");

        fixture.open(side, BIG_SIZE, 1_100 * ONE).expect("open");
        fixture.assert_i1("after the open");

        let market = fixture.market_state();
        let position = fixture.position_state();
        let (oi, count) = if side == SIDE_LONG {
            (market.long_oi_usd, market.long_positions)
        } else {
            (market.short_oi_usd, market.short_positions)
        };
        assert_eq!(
            oi, position.entry_notional_usd,
            "open interest is booked at entry notional"
        );
        assert_eq!(count, 1);
        assert_eq!(market.locked_quote, position.collateral_quote);
        assert_eq!(market.reserved_quote, position.reserve_quote);
        assert_eq!(fixture.pool_state().locked_quote, position.collateral_quote);
        assert_eq!(fixture.pool_state().reserved_quote, position.reserve_quote);

        // A profitable exit for this side, so the pool pays out and the
        // liabilities move downward rather than only up.
        fixture.set_price(exit_cents);
        fixture.close().expect("close");
        fixture.assert_i1("after the close");

        let market = fixture.market_state();
        assert_eq!(
            (
                market.long_oi_usd,
                market.short_oi_usd,
                market.long_positions,
                market.short_positions
            ),
            (0, 0, 0, 0),
            "every counter returns to exactly zero"
        );
        assert_eq!((market.locked_quote, market.reserved_quote), (0, 0));
        let pool = fixture.pool_state();
        assert_eq!((pool.locked_quote, pool.reserved_quote), (0, 0));
        assert!(
            pool.quote_deposited < deposited_before,
            "a winning trade must have cost the liquidity providers something"
        );
        assert!(!fixture.position_exists());
    }
}

/// The exit price uses the **position's** snapshotted spread, not the market's
/// live one.
///
/// Reading it live would let an admin retroactively tax every open exit, and —
/// once `confidence + spread >= mid` — revert the ones it could no longer price.
#[test]
fn the_exit_price_uses_the_positions_snapshotted_spread() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open at a 10 bps spread");
    let snapshot = fixture.position_state().spread_bps;
    assert_eq!(snapshot, 10);

    // The admin raises the market's spread fiftyfold after the fact.
    fixture
        .set_risk_params(
            fixture.market,
            RiskParams {
                spread_bps: 500,
                ..active_params()
            },
        )
        .expect("raise the spread");
    assert_eq!(fixture.market_state().spread_bps, 500);

    let mid = price_at(10_000);
    let logs = fixture.close().expect("close");
    let closed: PositionClosed = event(&logs);

    let with_snapshot =
        execution_price(Side::Long, PriceDirection::Close, mid, 0, snapshot).unwrap();
    let with_market_value =
        execution_price(Side::Long, PriceDirection::Close, mid, 0, 500).unwrap();
    assert_ne!(
        with_snapshot, with_market_value,
        "the two spreads must give different prices or this proves nothing"
    );
    assert_eq!(
        closed.exit_price, with_snapshot,
        "the exit must be struck off the snapshot, not the market's live spread"
    );
}

/// Liquidatability is judged at **current** notional, not entry notional.
///
/// A short whose price has risen carries a larger requirement than it opened
/// with. At entry notional it would carry the old, smaller one — so a position
/// that genuinely no longer meets maintenance margin would be refused by the
/// only liquidation path this milestone ships, and the pool would underwrite it
/// for free while the loss grew.
#[test]
fn liquidatability_is_judged_at_current_notional() {
    // No spread, so the entry and exit prices are the mid exactly and the
    // discriminating window below is a clean arithmetic statement.
    let mut fixture = Fixture::new(RiskParams {
        spread_bps: 0,
        max_oracle_drift_bps: 20,
        ..active_params()
    });
    fixture
        .open(SIDE_SHORT, BIG_SIZE, 1_100 * ONE)
        .expect("open a $10,000 short");
    let position = fixture.position_state();

    // Up 5.7%: inside the window where the entry-notional requirement is still
    // met and the current-notional one is not.
    fixture.set_price(10_570);
    let expected = expected_close(&fixture, price_at(10_570));

    assert!(
        !is_liquidatable(
            expected.equity_usd,
            position.entry_notional_usd,
            position.maintenance_margin_bps
        )
        .unwrap(),
        "at entry notional this position still meets maintenance margin — that \
         is what makes the test discriminating"
    );
    assert!(
        is_liquidatable(
            expected.equity_usd,
            expected.current_notional_usd,
            position.maintenance_margin_bps
        )
        .unwrap(),
        "at current notional it does not"
    );
    assert!(
        expected.current_notional_usd > position.entry_notional_usd,
        "the notional must actually have grown"
    );
    assert!(
        margin_requirement(
            expected.current_notional_usd,
            position.maintenance_margin_bps
        )
        .unwrap()
            > margin_requirement(position.entry_notional_usd, position.maintenance_margin_bps)
                .unwrap(),
        "and so must the requirement read off it"
    );

    fixture
        .admin_settle()
        .expect("a short whose notional has grown must be liquidatable");
    assert!(!fixture.position_exists());
    fixture.assert_i1("after the liquidation");
}

/// A position that still meets maintenance margin is not liquidatable, so the
/// gate above is a gate rather than a formality.
#[test]
fn a_healthy_position_cannot_be_liquidated() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");
    expect_error(
        fixture.admin_settle().map(|_| ()),
        PerpsError::PositionNotLiquidatable,
    );
    assert!(fixture.position_exists());
}

/// Δt is clamped at **both** ends.
///
/// A backwards `Clock::unix_timestamp` is a state the cluster genuinely reaches
/// — it is a stake-weighted vote estimate and is not monotonic. An unguarded
/// `as u64` on a negative interval is about 1.8e19 seconds, which even after the
/// settle-window clamp still moves both indices and, worse, writes
/// `last_settle_ts` backwards so the next honest call re-charges the whole gap.
/// The upper clamp is asserted in the same test, against the exact index the
/// window's worth of accrual produces.
#[test]
fn the_accrual_interval_is_clamped_at_both_ends() {
    // Borrow switched on, because an accrual of zero would make both halves
    // vacuously true.
    let mut fixture = Fixture::new(RiskParams {
        borrow_rate_per_hour: MAX_BORROW_RATE_PER_HOUR,
        funding_cap_per_hour: 1_000_000,
        funding_sensitivity: 1_000_000,
        ..active_params()
    });
    // A position, so the pool has reserve and utilisation is non-zero.
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    let before = fixture.market_state();
    assert!(fixture.pool_state().reserved_quote > 0);

    // ── the lower end ───────────────────────────────────────────────────────
    fixture.advance(-3_600, 0);
    fixture
        .settle_market()
        .expect("a backwards clock is a no-op, not an error");
    let after = fixture.market_state();
    assert_eq!(
        (
            after.cum_borrow_index,
            after.borrow_remainder_carry,
            after.cum_funding_index,
            after.last_settle_ts,
            after.last_rate_sample_ts,
        ),
        (
            before.cum_borrow_index,
            before.borrow_remainder_carry,
            before.cum_funding_index,
            before.last_settle_ts,
            before.last_rate_sample_ts,
        ),
        "a backwards clock must accrue nothing and write nothing"
    );

    // The same instant is the keeper-calls-twice case, and also a no-op.
    fixture.advance(3_600, 0);
    fixture.settle_market().expect("same instant");
    assert_eq!(fixture.market_state().last_settle_ts, before.last_settle_ts);

    // ── the upper end ───────────────────────────────────────────────────────
    let pool = fixture.pool_state();
    let utilisation = utilization_bps(
        u128::from(pool.reserved_quote),
        u128::from(pool.quote_deposited),
    )
    .unwrap();
    let window = u64::from(before.max_settle_window_seconds);
    let expected = borrow_index_delta(
        before.borrow_rate_per_hour,
        utilisation,
        window,
        before.borrow_remainder_carry,
    )
    .unwrap();
    assert!(
        expected.index_delta > 0,
        "the accrual has to be non-zero or neither half proves anything"
    );

    // Thirty days unattended.
    fixture.advance(30 * 24 * 60 * 60, 100);
    fixture.settle_market().expect("settle after a long gap");
    let after = fixture.market_state();
    assert_eq!(
        after.cum_borrow_index,
        before.cum_borrow_index + expected.index_delta,
        "only the settle window may be charged, never the whole gap"
    );
    // The timestamp still advances to now: the clamp bounds what is charged,
    // not what has elapsed. Otherwise the unaccrued remainder would simply be
    // charged by the next call.
    assert_eq!(after.last_settle_ts, fixture.now_unix);
}

/// Permissionless listing is safe because a market is born quarantined, and a
/// quarantined market refuses every position.
///
/// `max_oi_usd == 0` **is** the quarantine — there is no separate flag, so there
/// is no state in which a flag says tradeable while the risk parameters are
/// still zero. The second market here is listed by the trader, who is not the
/// admin, and receives nothing for it.
#[test]
fn a_market_is_born_quarantined_and_refuses_new_risk() {
    let mut fixture = Fixture::new(active_params());

    // Listed permissionlessly and never activated.
    let price_update = fixture.other_price_update(OTHER_FEED_ID);
    fixture.write_price(price_update, OTHER_FEED_ID, 10_000);
    fixture
        .qualify_feed(OTHER_FEED_ID)
        .expect("qualify the second feed");
    fixture
        .create_market(OTHER_FEED_ID)
        .expect("anyone may list a market");

    let fresh = fixture.market_state_at(pda(&[b"market", OTHER_FEED_ID.as_ref()]));
    assert_eq!(fresh.max_oi_usd, 0, "born quarantined");
    assert_eq!(
        (
            fresh.initial_margin_bps,
            fresh.maintenance_margin_bps,
            fresh.max_profit_bps,
            fresh.open_fee_bps,
            fresh.close_fee_bps,
        ),
        (0, 0, 0, 0, 0),
        "every risk parameter starts at zero, so listing grants nothing"
    );
    assert_eq!(
        fresh.quarantined_ts, fixture.now_unix,
        "the wind-down clock starts at creation"
    );

    // And the activated market, once quarantined, refuses an open.
    fixture
        .set_risk_params(fixture.market, quarantined_params(active_params()))
        .expect("quarantine");
    expect_error(
        fixture.open(SIDE_LONG, BIG_SIZE, 1_100 * ONE),
        PerpsError::MarketQuarantined,
    );
}

/// The open-interest cap binds on the side being added to.
#[test]
fn the_open_interest_cap_is_enforced() {
    let mut fixture = Fixture::new(RiskParams {
        // Below the $10,000 the position below would add.
        max_oi_usd: 5_000 * USD_SCALE,
        ..active_params()
    });
    expect_error(
        fixture.open(SIDE_LONG, BIG_SIZE, 1_100 * ONE),
        PerpsError::OpenInterestCapExceeded,
    );
}

/// Lowering the utilisation ceiling below current utilisation blocks **new
/// risk** and invalidates **no open position**.
///
/// This is the split between I2's two forms, and getting it wrong bricks the
/// protocol rather than merely being untidy. `open_position` and `lp_withdraw`
/// are judged against the ceiling in force; the three settlement paths and
/// `lp_deposit` are judged against the pre-state instead. Judging a close by the
/// ceiling would mean an admin who lowered it below current utilisation had
/// reverted every exit — and, because `set_pool_limits` caps at
/// `M5_MAX_UTILIZATION_BPS`, could not raise it back, while `lp_deposit` (the
/// only other way utilisation falls) would be reverting too.
#[test]
fn lowering_the_utilisation_ceiling_blocks_new_risk_but_not_exits() {
    let mut fixture = Fixture::new(active_params());

    // The setter's own bounds first: strictly inside `(0, M5_MAX_UTILIZATION_BPS]`.
    for refused in [0, sakura_perps::M5_MAX_UTILIZATION_BPS + 1, 10_000] {
        expect_error(
            fixture.set_pool_limits(100 * LP_LIQUIDITY, refused),
            PerpsError::UtilizationCeilingTooHigh,
        );
    }
    fixture
        .set_pool_limits(100 * LP_LIQUIDITY, sakura_perps::M5_MAX_UTILIZATION_BPS)
        .expect("the bound itself is accepted, so the range is inclusive");

    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open while there is room");
    let pool = fixture.pool_state();
    let utilisation = utilization_bps(
        u128::from(pool.reserved_quote),
        u128::from(pool.quote_deposited),
    )
    .unwrap();
    assert!(utilisation > 1, "the ceiling below has to actually bite");

    // Lowered below where the pool already is. Permitted, deliberately, and
    // with no check on the open book.
    fixture
        .set_pool_limits(100 * LP_LIQUIDITY, 1)
        .expect("a lowering below current utilisation is permitted");

    // New risk is refused. A newcomer, because one position per owner per
    // market means the fixture's trader could not open again regardless.
    let (newcomer, newcomer_tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        newcomer_tokens,
        SIDE_LONG,
        BIG_SIZE,
        1_100 * ONE,
        unbounded_limit(SIDE_LONG),
    );
    expect_error(
        fixture.send(instruction, &[&newcomer]),
        PerpsError::UtilizationTooHigh,
    );

    // The exit is not. This is the assertion the whole distinction exists for.
    fixture
        .close()
        .expect("a lowered ceiling must not trap an open position");
    fixture.assert_i1("after closing under a lowered ceiling");
    assert_eq!(fixture.pool_state().reserved_quote, 0);
}

/// Slippage protection is mandatory on both legs, and zero is rejected rather
/// than read as "unset".
///
/// One field rather than two, because only one of a pair is ever read and a
/// field that is never read still looks checked. Zero has to be an explicit
/// rejection: as a ceiling it refuses every price, but as a floor it would
/// accept every price, so the two readings disagree about what "unset" means.
#[test]
fn slippage_bounds_are_mandatory_and_enforced_on_both_legs() {
    let mut fixture = Fixture::new(active_params());

    let zero_limit = fixture.open_ix_for(
        fixture.trader.pubkey(),
        fixture.trader_token_account,
        SIDE_LONG,
        BIG_SIZE,
        1_100 * ONE,
        0,
    );
    let trader = fixture.trader.insecure_clone();
    expect_error(
        fixture.send(zero_limit, &[&trader]),
        PerpsError::SlippageExceeded,
    );

    // A long pays up, so a ceiling one unit below the execution price refuses.
    let entry = execution_price(Side::Long, PriceDirection::Open, price_at(10_000), 0, 10).unwrap();
    let too_tight = fixture.open_ix_for(
        fixture.trader.pubkey(),
        fixture.trader_token_account,
        SIDE_LONG,
        BIG_SIZE,
        1_100 * ONE,
        entry - 1,
    );
    expect_error(
        fixture.send(too_tight, &[&trader]),
        PerpsError::SlippageExceeded,
    );

    // Exactly the execution price is accepted, so the bound is inclusive rather
    // than the whole range having been shifted by one.
    let exact = fixture.open_ix_for(
        fixture.trader.pubkey(),
        fixture.trader_token_account,
        SIDE_LONG,
        BIG_SIZE,
        1_100 * ONE,
        entry,
    );
    fixture.send(exact, &[&trader]).expect("open at the limit");
    assert_eq!(fixture.position_state().entry_price, entry);

    // And the exit leg mirrors it: a long receives, so a floor one unit above
    // the exit price refuses.
    let exit = execution_price(Side::Long, PriceDirection::Close, price_at(10_000), 0, 10).unwrap();
    let instruction = fixture.close_ix_with(exit + 1);
    expect_error(
        fixture.send(instruction, &[&trader]),
        PerpsError::SlippageExceeded,
    );
    let instruction = fixture.close_ix_with(exit);
    fixture
        .send(instruction, &[&trader])
        .expect("close at the limit");
}

/// Collateral that does not meet the initial margin once the open fee is taken
/// is refused, and the fee is taken first.
#[test]
fn margin_is_checked_on_what_the_open_fee_left() {
    let mut fixture = Fixture::new(active_params());
    // $10,010 of notional needs $1,001 of margin plus $10.01 of fee. A dollar
    // short of that total is refused.
    expect_error(
        fixture.open(SIDE_LONG, BIG_SIZE, 1_010 * ONE),
        PerpsError::InsufficientMargin,
    );
    // And a dollar over it is not.
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_012 * ONE)
        .expect("margin met once the fee is paid");
    fixture.assert_i1("after a marginal open");
}

// ════════════════════════════════════════════════════════════════════════════
// Authority. Every admin-gated instruction must refuse a stranger.
// ════════════════════════════════════════════════════════════════════════════

/// A feed id used only by the authority test, so it collides with nothing.
const AUTHORITY_FEED_ID: [u8; 32] = [21u8; 32];

/// The five market- and pool-level admin instructions, each refused for a
/// stranger and then accepted from the real admin.
///
/// `address = exchange.admin @ NotAdmin` is the whole of the authorisation
/// model — there is no other check anywhere — and it is one line per
/// instruction. Replace it with the tautology `address = admin.key()` and every
/// one of these becomes permissionless: a stranger can halt the exchange with
/// `set_pause_flags`, quarantine any market with `set_risk_params`, or strand
/// liquidity by collapsing the pool's limits.
///
/// The positive control after each refusal is what makes the refusal mean
/// something: without it a bad account list, an unfunded payer or a stale
/// blockhash would read as "refused" just as well.
#[test]
fn admin_only_market_and_pool_instructions_refuse_a_stranger() {
    let mut fixture = Fixture::new(active_params());
    let stranger = fixture.stranger();
    let admin = fixture.admin.pubkey();

    // 1. set_pause_flags — halting the exchange.
    let mut instruction = fixture.set_pause_flags_ix(PauseFlags::ALL);
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error_at(
        "set_pause_flags: ",
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert_eq!(
        fixture.exchange_state().paused_flags,
        0,
        "a stranger must not be able to halt the exchange"
    );
    fixture.set_pause_flags(PauseFlags::ALL);
    assert_eq!(fixture.exchange_state().paused_flags, PauseFlags::ALL);
    fixture.set_pause_flags(0);

    // 2. qualify_feed — deciding what this exchange is willing to price.
    let price_update = fixture.other_price_update(AUTHORITY_FEED_ID);
    fixture.write_price(price_update, AUTHORITY_FEED_ID, 10_000);
    let mut instruction =
        fixture.qualify_feed_ix(AUTHORITY_FEED_ID, feed_params(AUTHORITY_FEED_ID));
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error_at(
        "qualify_feed: ",
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert!(
        fixture
            .svm
            .get_account(&pda(&[b"feed", AUTHORITY_FEED_ID.as_ref()]))
            .is_none_or(|account| account.data.is_empty()),
        "the refused qualification must not have created the feed"
    );
    fixture
        .qualify_feed(AUTHORITY_FEED_ID)
        .expect("the admin may qualify a feed");

    // 3. set_feed_revoked — shutting off new risk against a feed.
    let feed = fixture.feed;
    let mut instruction = fixture.set_feed_revoked_ix(feed, true);
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error_at(
        "set_feed_revoked: ",
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert!(!fixture.feed_state().revoked);
    fixture.set_feed_revoked(true);
    assert!(fixture.feed_state().revoked);
    fixture.set_feed_revoked(false);

    // 4. set_risk_params — including the quarantine, which is the precondition
    //    for the emergency force-close.
    let market = fixture.market;
    let mut instruction = fixture.set_risk_params_ix(market, quarantined_params(active_params()));
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error_at(
        "set_risk_params: ",
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert!(
        !fixture.market_state().is_quarantined(),
        "a stranger must not be able to quarantine a market"
    );
    fixture
        .set_risk_params(market, quarantined_params(active_params()))
        .expect("the admin may quarantine");
    assert!(fixture.market_state().is_quarantined());

    // 5. set_pool_limits.
    let mut instruction = fixture.set_pool_limits_ix(1, 1);
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error_at(
        "set_pool_limits: ",
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert_eq!(
        fixture.pool_state().max_utilization_bps,
        sakura_perps::M5_MAX_UTILIZATION_BPS,
        "a stranger must not be able to collapse the pool's limits"
    );
    fixture
        .set_pool_limits(1, 1)
        .expect("the admin may set pool limits");
    assert_eq!(fixture.pool_state().max_utilization_bps, 1);
}

/// `admin_settle_position` refuses a stranger.
///
/// The payout still routes to `position.owner`, so what this closes is not
/// theft — it is that any keypair on the cluster could liquidate any position
/// the moment it dipped below maintenance margin. M5 deliberately ships no
/// keeper liquidation; without this constraint it ships one by accident, open
/// to everybody.
#[test]
fn admin_settle_position_refuses_a_stranger() {
    let mut fixture = Fixture::new(RiskParams {
        spread_bps: 0,
        max_oracle_drift_bps: 20,
        ..active_params()
    });
    fixture
        .open(SIDE_SHORT, BIG_SIZE, 1_100 * ONE)
        .expect("open a $10,000 short");
    // Up 5.7%: liquidatable at current notional, so `PositionNotLiquidatable`
    // cannot be what refuses the stranger.
    fixture.set_price(10_570);

    let stranger = fixture.stranger();
    let admin = fixture.admin.pubkey();
    let mut instruction = fixture.admin_settle_ix();
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error(
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert!(
        fixture.position_exists(),
        "a stranger must not be able to liquidate"
    );
    fixture.assert_i1("after the refused liquidation");

    fixture
        .admin_settle()
        .expect("the exchange admin may liquidate the same position");
    assert!(!fixture.position_exists());
    fixture.assert_i1("after the liquidation");
}

/// `emergency_close_position` refuses a stranger.
///
/// This is the instruction with the fewest gates in the program — no pause
/// flag, no oracle, no feed, no slippage bound, no liquidatability test, and
/// the owner is not a signer. Once the market is quarantined and the day has
/// passed, the admin constraint is the only thing left, and a stranger through
/// it could force-exit every trader in the market at a moment of their own
/// choosing.
#[test]
fn emergency_close_position_refuses_a_stranger() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");
    let market = fixture.market;
    fixture
        .set_risk_params(market, quarantined_params(active_params()))
        .expect("quarantine");
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);

    let stranger = fixture.stranger();
    let admin = fixture.admin.pubkey();
    let mut instruction = fixture.emergency_close_ix();
    substitute(&mut instruction, admin, stranger.pubkey());
    expect_error(
        fixture.send(instruction, &[&stranger]),
        PerpsError::NotAdmin,
    );
    assert!(
        fixture.position_exists(),
        "a stranger must not be able to force a position out"
    );

    fixture
        .emergency_close()
        .expect("the exchange admin may wind the same position down");
    assert!(!fixture.position_exists());
    fixture.assert_i1("after the wind-down");
}

/// **B2's other half.** The emergency exit refuses a market that is *not*
/// quarantined.
///
/// `constraint = market.is_quarantined() @ MarketNotQuarantined` is the single
/// line separating a wind-down instruction from an unconditional admin
/// force-close, and the handler's 24-hour delay does not back it up: leaving
/// quarantine sets `quarantined_ts = 0`, so on every live market
/// `now - quarantined_ts` is over fifty years and the delay check passes
/// trivially. This test asserts that state explicitly before the call, so the
/// refusal is attributable to the constraint and to nothing else.
///
/// Weaken the constraint and an admin can close any position on any healthy,
/// fully-trading market at `market.last_good_price` — with no pause gate, no
/// oracle read, no owner signature, no slippage bound and no liquidatability
/// test in the way. That is a bypass of every exit control the milestone ships.
#[test]
fn the_emergency_exit_refuses_a_market_that_is_not_quarantined() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open");

    let market = fixture.market_state();
    assert!(!market.is_quarantined(), "the market is live");
    assert_eq!(
        market.quarantined_ts, 0,
        "activation zeroes the wind-down clock"
    );

    // A day passes, so the delay gate is provably satisfied.
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);
    assert!(
        fixture.now_unix - fixture.market_state().quarantined_ts >= EMERGENCY_CLOSE_DELAY_SECONDS,
        "the delay must not be what refuses this, or the test proves nothing"
    );

    let trader_before = fixture.token_balance(fixture.trader_token_account);
    expect_error(
        fixture.emergency_close().map(|_| ()),
        PerpsError::MarketNotQuarantined,
    );
    assert!(
        fixture.position_exists(),
        "a live market's positions must not be force-closed"
    );
    assert_eq!(
        fixture.token_balance(fixture.trader_token_account),
        trader_before,
        "and no value may move"
    );
    fixture.assert_i1("after the refusal");

    // The mirror. Quarantine, wait out the delay, and the identical call goes
    // through — so the refusal above was the constraint and not the fixture.
    let market = fixture.market;
    fixture
        .set_risk_params(market, quarantined_params(active_params()))
        .expect("quarantine");
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);
    fixture
        .emergency_close()
        .expect("a quarantined market past its delay may be wound down");
    assert!(!fixture.position_exists());
    fixture.assert_i1("after the wind-down");
}

// ════════════════════════════════════════════════════════════════════════════
// The snapshotted spread, on the two exits the trader does not sign.
// ════════════════════════════════════════════════════════════════════════════

/// `admin_settle_position` strikes its exit off `position.spread_bps`.
///
/// This path and the emergency path are the two where the snapshot matters most
/// and where it was untested: neither takes a `limit_price`, so the trader has
/// no slippage bound to protect them, and neither requires the trader to sign.
/// An admin who could widen `market.spread_bps` to the 500 bps ceiling and then
/// liquidate would move the exit against the trader by up to 5% of notional, on
/// a position they cannot defend.
#[test]
fn the_liquidation_exit_price_uses_the_positions_snapshotted_spread() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_SHORT, BIG_SIZE, 1_100 * ONE)
        .expect("open a short at a 10 bps spread");
    let snapshot = fixture.position_state().spread_bps;
    assert_eq!(snapshot, 10);

    let market = fixture.market;
    fixture
        .set_risk_params(
            market,
            RiskParams {
                spread_bps: MAX_SPREAD_BPS,
                ..active_params()
            },
        )
        .expect("widen the market's spread to its ceiling");
    assert_eq!(fixture.market_state().spread_bps, MAX_SPREAD_BPS);

    fixture.set_price(10_570);
    let mid = price_at(10_570);
    let logs = fixture
        .admin_settle()
        .expect("the short is liquidatable at current notional");
    let closed: PositionClosed = event(&logs);

    let with_snapshot =
        execution_price(Side::Short, PriceDirection::Close, mid, 0, snapshot).unwrap();
    let with_market_value =
        execution_price(Side::Short, PriceDirection::Close, mid, 0, MAX_SPREAD_BPS).unwrap();
    assert_ne!(
        with_snapshot, with_market_value,
        "the two spreads must give different prices or this proves nothing"
    );
    assert_eq!(
        closed.exit_price, with_snapshot,
        "a liquidation must be struck off the snapshot, not the market's live spread"
    );
    fixture.assert_i1("after the liquidation");
}

/// `emergency_close_position` strikes its exit off `position.spread_bps`.
///
/// The B2 test cannot see this distinction: `quarantined_params` changes only
/// `max_oi_usd`, so there the market's spread and the position's snapshot are
/// equal by construction and either source gives the same answer. Here the
/// quarantine and the widening land in one retune, which is exactly the shape
/// an admin winding a market down would use.
#[test]
fn the_emergency_exit_price_uses_the_positions_snapshotted_spread() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open a long at a 10 bps spread");
    let snapshot = fixture.position_state().spread_bps;
    assert_eq!(snapshot, 10);

    let market = fixture.market;
    fixture
        .set_risk_params(
            market,
            RiskParams {
                spread_bps: MAX_SPREAD_BPS,
                max_oi_usd: 0,
                ..active_params()
            },
        )
        .expect("quarantine at the widest legal spread");
    assert_eq!(fixture.market_state().spread_bps, MAX_SPREAD_BPS);
    fixture.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);

    let reference = fixture.market_state().last_good_price;
    assert_eq!(reference, price_at(10_000));

    let logs = fixture.emergency_close().expect("wind the position down");
    let closed: PositionClosed = event(&logs);

    let with_snapshot =
        execution_price(Side::Long, PriceDirection::Close, reference, 0, snapshot).unwrap();
    let with_market_value = execution_price(
        Side::Long,
        PriceDirection::Close,
        reference,
        0,
        MAX_SPREAD_BPS,
    )
    .unwrap();
    assert_ne!(
        with_snapshot, with_market_value,
        "the two spreads must give different prices or this proves nothing"
    );
    assert_eq!(
        closed.exit_price, with_snapshot,
        "a wind-down must be struck off the snapshot, not the market's live spread"
    );
    fixture.assert_i1("after the wind-down");
}

// ════════════════════════════════════════════════════════════════════════════
// Configuration validation — the gates that stop an admin listing an unsafe
// market in the first place.
// ════════════════════════════════════════════════════════════════════════════

/// The seventeen risk parameters as the market recorded them.
///
/// Returned as a flat vector because a seventeen-element tuple has neither
/// `Debug` nor `PartialEq`, and the point of reading them back is to compare
/// the whole block before and after a run of refused retunes.
fn risk_snapshot(market: &Market) -> Vec<u128> {
    vec![
        market.initial_margin_bps.into(),
        market.maintenance_margin_bps.into(),
        market.liquidation_fee_bps.into(),
        market.max_profit_bps.into(),
        market.spread_bps.into(),
        market.open_fee_bps.into(),
        market.close_fee_bps.into(),
        market.max_oi_usd,
        market.max_oracle_drift_bps.into(),
        market.min_position_size_base.into(),
        market.min_notional_usd,
        market.min_collateral_usd,
        market.borrow_rate_per_hour,
        market.funding_sensitivity,
        market.funding_cap_per_hour,
        market.max_settle_window_seconds.into(),
        market.min_settle_interval_seconds.into(),
    ]
}

/// Every one of `set_risk_params`'s validations, one perturbed field at a time,
/// and nothing written by any of them.
///
/// These are the guards that stop an admin putting a live market into a
/// configuration nobody can trade safely, and several encode cross-parameter
/// relations no reader would reconstruct from the field names: fees that must
/// dominate an hour of funding, fees that must dominate the oracle-staleness
/// option, a profit cap the pool can actually fund. The risk crate proves those
/// predicates in isolation; what is asserted here is that the instruction still
/// calls them and still maps each failure to its own variant.
///
/// The state comparison at the end is the second half of the claim: a refused
/// retune must leave all seventeen fields alone, not write some and revert on a
/// later `require!`.
#[test]
fn every_risk_parameter_validation_refuses_and_writes_nothing() {
    let mut fixture = Fixture::new(active_params());
    let before = risk_snapshot(&fixture.market_state());

    let rows: Vec<(&str, RiskParams, PerpsError)> = vec![
        (
            "1: initial margin no greater than maintenance plus the liquidation fee",
            RiskParams {
                maintenance_margin_bps: 900,
                ..active_params()
            },
            PerpsError::InvalidMarginParameters,
        ),
        (
            "2: spread above its ceiling",
            RiskParams {
                spread_bps: MAX_SPREAD_BPS + 1,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "2: open fee above its ceiling",
            RiskParams {
                open_fee_bps: MAX_TRADE_FEE_BPS + 1,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "2: close fee above its ceiling",
            RiskParams {
                close_fee_bps: MAX_TRADE_FEE_BPS + 1,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "3: a zero profit cap",
            RiskParams {
                max_profit_bps: 0,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            // Validation 3 also forbids `initial_margin_bps == 0` — infinite
            // leverage — but that clause is **unreachable**, and this row is
            // what establishes it: validation 1 runs first and demands
            // `initial > maintenance + liquidation_fee`, which already implies
            // `initial > 0`. Zero initial margin is therefore refused as
            // `InvalidMarginParameters`, not as the variant the later check
            // would have produced. Nothing is wrong — the clause is a
            // belt-and-braces duplicate — but a reader chasing
            // `InvalidRiskParameters` should not expect to find it here.
            "1: zero initial margin, which validation 1 catches before 3 can",
            RiskParams {
                initial_margin_bps: 0,
                maintenance_margin_bps: 0,
                liquidation_fee_bps: 0,
                ..active_params()
            },
            PerpsError::InvalidMarginParameters,
        ),
        (
            // 100 bps of funding an hour against a 20 bps round trip: holding
            // would cost less than trading, which is the arbitrage this forbids.
            "4: funding that outruns a round trip's fees",
            RiskParams {
                funding_cap_per_hour: MAX_FUNDING_RATE_PER_HOUR,
                ..active_params()
            },
            PerpsError::FeesDoNotDominateFunding,
        ),
        (
            "5: a zero settle window, which accrues nothing ever",
            RiskParams {
                max_settle_window_seconds: 0,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "5: a settle window past its ceiling",
            RiskParams {
                max_settle_window_seconds: MAX_SETTLE_WINDOW_SECONDS + 1,
                ..active_params()
            },
            PerpsError::SettleWindowTooLong,
        ),
        (
            "6: a zero minimum position size",
            RiskParams {
                min_position_size_base: 0,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "6: a zero minimum notional",
            RiskParams {
                min_notional_usd: 0,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "6: a zero minimum collateral",
            RiskParams {
                min_collateral_usd: 0,
                ..active_params()
            },
            PerpsError::InvalidRiskParameters,
        ),
        (
            "7: a borrow rate past its ceiling",
            RiskParams {
                borrow_rate_per_hour: MAX_BORROW_RATE_PER_HOUR + 1,
                ..active_params()
            },
            PerpsError::BorrowRateTooHigh,
        ),
        (
            // The fees are raised to their own ceiling so validation 4 passes
            // and validation 7 is what this row actually reaches.
            "7: a funding cap past its ceiling",
            RiskParams {
                open_fee_bps: MAX_TRADE_FEE_BPS,
                close_fee_bps: MAX_TRADE_FEE_BPS,
                funding_cap_per_hour: MAX_FUNDING_RATE_PER_HOUR + 1,
                ..active_params()
            },
            PerpsError::FundingRateTooHigh,
        ),
        (
            "7: a funding sensitivity past its ceiling",
            RiskParams {
                funding_sensitivity: MAX_FUNDING_SENSITIVITY + 1,
                ..active_params()
            },
            PerpsError::FundingSensitivityTooHigh,
        ),
        (
            "8: a profit cap the pool's reserve budget cannot fund",
            RiskParams {
                max_profit_bps: MAX_RESERVE_LEVERAGE * active_params().initial_margin_bps + 1,
                ..active_params()
            },
            PerpsError::ReserveLeverageTooHigh,
        ),
        (
            // 10 + 10 + 2 x 10 = 40 bps of round trip against 41 bps of drift:
            // the staleness option pays for itself by one basis point.
            "9: an oracle-drift allowance the fees no longer dominate",
            RiskParams {
                max_oracle_drift_bps: 41,
                ..active_params()
            },
            PerpsError::FeesDoNotDominateDrift,
        ),
    ];

    let market = fixture.market;
    for (why, params, expected) in rows {
        let result = fixture.set_risk_params(market, params);
        expect_error_at(&format!("{why} — "), result, expected);
    }

    assert_eq!(
        risk_snapshot(&fixture.market_state()),
        before,
        "a refused retune must write nothing at all"
    );

    // The positive control: the same instruction, one legal field changed.
    fixture
        .set_risk_params(
            market,
            RiskParams {
                close_fee_bps: 20,
                ..active_params()
            },
        )
        .expect("a valid retune must still apply");
    assert_eq!(fixture.market_state().close_fee_bps, 20);
}

/// A feed id paired with the parameters to qualify it under.
///
/// Named so the case table below does not nest a tuple inside a tuple, which
/// trips `clippy::type_complexity` under `-D warnings`. Aliasing it in one place
/// also keeps `feed_row`'s return type and the table that consumes it in step by
/// construction, rather than by remembering to edit both.
type FeedCase = ([u8; 32], QualifyFeedParams);

/// Build a feed-qualification row: a distinct feed id and a copy of the shared
/// parameters with one field perturbed.
///
/// A distinct id per row is mandatory, not tidiness. The `feed` PDA is `init`,
/// so a reused id fails with the System Program's `AccountAlreadyInUse` while
/// Anchor is still walking the account list — before the validation under test
/// has run at all.
fn feed_row(id: u8, mutate: impl FnOnce(&mut QualifyFeedParams)) -> FeedCase {
    let feed_id = [id; 32];
    let mut params = feed_params(feed_id);
    mutate(&mut params);
    (feed_id, params)
}

/// Every validation `qualify_feed` performs before it will price anything.
///
/// Guard ordering is the one worth naming: trading guards looser than
/// liquidation guards make positions openable at prices they cannot be
/// liquidated at, which inverts the entire reason the two guard sets exist.
/// `ConfidenceGateTooWide` is the other — it is not a policy preference but the
/// condition under which `execution_price` is total, and getting it wrong
/// produces an exit that cannot be priced at all rather than one priced badly.
#[test]
fn every_feed_qualification_validation_refuses() {
    let mut fixture = Fixture::new(active_params());

    let rows: Vec<(&str, FeedCase, PerpsError)> = vec![
        (
            "1: an exponent below the range normalize_price can apply",
            feed_row(30, |p| p.expected_exponent = MIN_EXPONENT - 1),
            PerpsError::InvalidFeedParameters,
        ),
        (
            "1: an exponent above it",
            feed_row(31, |p| p.expected_exponent = MAX_EXPONENT + 1),
            PerpsError::InvalidFeedParameters,
        ),
        (
            "2: a zero lower sanity bound",
            feed_row(32, |p| p.min_price = 0),
            PerpsError::InvalidFeedParameters,
        ),
        (
            "2: a band that is not a band",
            feed_row(33, |p| p.min_price = p.max_price),
            PerpsError::InvalidFeedParameters,
        ),
        (
            "2: asset decimals past the ceiling",
            feed_row(34, |p| p.asset_decimals = MAX_ASSET_DECIMALS + 1),
            PerpsError::InvalidFeedParameters,
        ),
        (
            "3: a zero divergence tolerance",
            feed_row(35, |p| p.max_divergence_bps = 0),
            PerpsError::InvalidFeedParameters,
        ),
        (
            // At 10 000 bps the lower edge of the exit clamp band is zero, and
            // a mid clamped to zero cannot be valued.
            "3: a 100% divergence tolerance",
            feed_row(36, |p| p.max_divergence_bps = 10_000),
            PerpsError::InvalidFeedParameters,
        ),
        (
            "6: a confidence gate that plus the widest spread reaches 100%",
            feed_row(37, |p| {
                p.liquidation_max_confidence_bps = 10_000 - MAX_SPREAD_BPS
            }),
            PerpsError::ConfidenceGateTooWide,
        ),
        (
            "4: liquidation staleness tighter than trading staleness",
            feed_row(38, |p| p.liquidation_max_age_seconds = 10),
            PerpsError::GuardsNotOrdered,
        ),
        (
            "4: liquidation slot age tighter than trading slot age",
            feed_row(39, |p| p.liquidation_max_age_slots = 10),
            PerpsError::GuardsNotOrdered,
        ),
        (
            "4: liquidation confidence tighter than trading confidence",
            feed_row(40, |p| p.liquidation_max_confidence_bps = 50),
            PerpsError::GuardsNotOrdered,
        ),
        (
            "4: liquidation future skew tighter than trading future skew",
            feed_row(41, |p| p.liquidation_max_future_skew_seconds = 1),
            PerpsError::GuardsNotOrdered,
        ),
    ];

    for (why, (feed_id, params), expected) in rows {
        // A live, in-band price account for every row, so validation 5 — which
        // runs last — is never what refuses the row under test.
        let price_update = fixture.other_price_update(feed_id);
        fixture.write_price(price_update, feed_id, 10_000);
        let result = fixture.qualify_feed_with(feed_id, params);
        expect_error_at(&format!("{why} — "), result, expected);
        assert!(
            fixture
                .svm
                .get_account(&pda(&[b"feed", feed_id.as_ref()]))
                .is_none_or(|account| account.data.is_empty()),
            "{why}: a refused qualification must leave no feed behind"
        );
    }

    // The positive control: unperturbed, the same call qualifies.
    let feed_id = [42u8; 32];
    let price_update = fixture.other_price_update(feed_id);
    fixture.write_price(price_update, feed_id, 10_000);
    fixture
        .qualify_feed(feed_id)
        .expect("valid parameters must still qualify");
}

// ════════════════════════════════════════════════════════════════════════════
// The gates on the way into a position.
// ════════════════════════════════════════════════════════════════════════════

/// `PauseFlags::OPEN_POSITION` stops new risk and nothing else.
///
/// This is the admin's only brake during an incident, and if it silently
/// stopped working the failure would surface as losses rather than as a test
/// failure. The close in the same state is half the point: a pause that also
/// trapped existing positions would be a far worse instrument than the one
/// intended, and the B2 test — which sets every flag at once — cannot tell the
/// two bits apart.
#[test]
fn opening_is_refused_while_trading_is_paused_and_closing_is_not() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open before the pause");

    fixture.set_pause_flags(PauseFlags::OPEN_POSITION);
    let (newcomer, tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        tokens,
        SIDE_LONG,
        BIG_SIZE,
        1_100 * ONE,
        unbounded_limit(SIDE_LONG),
    );
    expect_error(
        fixture.send(instruction, &[&newcomer]),
        PerpsError::TradingPaused,
    );

    fixture
        .close()
        .expect("closing is a different flag and must be unaffected");
    assert!(!fixture.position_exists());
    fixture.assert_i1("after the close under an opening pause");
}

/// A spot price too far from its own EMA is refused at open, from both sides of
/// the boundary.
///
/// Opening is the only leg that rejects on an oracle condition — every exit
/// clamps symmetrically instead — and it is the gate against opening at a
/// manipulated spot while the EMA lags. Without it an attacker who can move
/// spot for a single slot opens at the manipulated entry and exits at the
/// clamped mid, taking the difference out of the pool on every position.
///
/// Both sides are asserted because only the pair is discriminating: a test that
/// merely refused a diverged price would pass just as well against an
/// `open_position` that refused everything.
#[test]
fn a_spot_price_diverging_from_its_ema_is_refused_at_open() {
    let mut fixture = Fixture::new(active_params());
    let tolerance = fixture.market_state().max_divergence_bps;
    assert_eq!(tolerance, 500, "5%, copied from the feed");

    // 501 bps above the EMA.
    assert!(diverges_beyond(price_at(10_501), price_at(10_000), tolerance).unwrap());
    fixture.set_price_with_ema(10_501, 10_000);
    let (over, tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        over.pubkey(),
        tokens,
        SIDE_LONG,
        BIG_SIZE,
        1_200 * ONE,
        unbounded_limit(SIDE_LONG),
    );
    expect_error(
        fixture.send(instruction, &[&over]),
        PerpsError::PriceDiverged,
    );

    // 499 bps: one basis point the other side of the same boundary.
    assert!(!diverges_beyond(price_at(10_499), price_at(10_000), tolerance).unwrap());
    fixture.set_price_with_ema(10_499, 10_000);
    let (under, tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        under.pubkey(),
        tokens,
        SIDE_LONG,
        BIG_SIZE,
        1_200 * ONE,
        unbounded_limit(SIDE_LONG),
    );
    fixture
        .send(instruction, &[&under])
        .expect("just inside the tolerance must open");
    fixture.assert_i1("after the open inside the tolerance");
}

/// A side that is neither long nor short is refused.
///
/// The field is a `u8` on the wire and every later branch reads it as though it
/// were a two-valued enum — `if side == SIDE_LONG { .. } else { .. }` treats
/// anything else as a short.
#[test]
fn an_unknown_side_is_refused() {
    let mut fixture = Fixture::new(active_params());
    let (newcomer, tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        tokens,
        2,
        BIG_SIZE,
        1_100 * ONE,
        // Non-zero, so the slippage check that follows cannot be what refuses
        // this.
        1,
    );
    expect_error(
        fixture.send(instruction, &[&newcomer]),
        PerpsError::InvalidPositionSide,
    );
}

/// Each of the three minimums refuses a position below it, one at a time.
///
/// All three raise `PositionTooSmall`, so the only way to show that all three
/// are checked is to violate exactly one per row — which means retuning the
/// market between rows rather than shrinking the order. Dust positions are free
/// to create and expensive to carry: each still accrues, still reserves, and
/// still costs an admin a transaction to settle.
#[test]
fn each_minimum_refuses_a_position_below_it() {
    // Five units at $100 is roughly $500 of notional on $100 of collateral —
    // comfortably above every minimum in `active_params`, so each row below
    // fails on the one minimum it raises and on nothing else.
    const SIZE: u64 = 5 * ONE_UNIT;
    const COLLATERAL: u64 = 100 * ONE;

    let mut fixture = Fixture::new(active_params());
    let market = fixture.market;

    let rows: Vec<(&str, RiskParams)> = vec![
        (
            "below the minimum position size",
            RiskParams {
                min_position_size_base: 10 * ONE_UNIT,
                ..active_params()
            },
        ),
        (
            "below the minimum notional",
            RiskParams {
                min_notional_usd: 2_000 * USD_SCALE,
                ..active_params()
            },
        ),
        (
            "below the minimum collateral",
            RiskParams {
                min_collateral_usd: 1_000 * USD_SCALE,
                ..active_params()
            },
        ),
    ];

    for (why, params) in rows {
        fixture
            .set_risk_params(market, params)
            .expect("retune the minimums");
        let (newcomer, tokens) = fixture.newcomer();
        let instruction = fixture.open_ix_for(
            newcomer.pubkey(),
            tokens,
            SIDE_LONG,
            SIZE,
            COLLATERAL,
            unbounded_limit(SIDE_LONG),
        );
        expect_error_at(
            &format!("{why} — "),
            fixture.send(instruction, &[&newcomer]),
            PerpsError::PositionTooSmall,
        );
    }

    // The positive control: with the minimums back where they started, the
    // identical order goes through.
    fixture
        .set_risk_params(market, active_params())
        .expect("restore the minimums");
    let (newcomer, tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        tokens,
        SIDE_LONG,
        SIZE,
        COLLATERAL,
        unbounded_limit(SIDE_LONG),
    );
    fixture
        .send(instruction, &[&newcomer])
        .expect("the same order must open once the minimums allow it");
    fixture.assert_i1("after the open");
}

/// A deposit that moves no tokens is refused.
///
/// The vault's balance delta — not the requested amount — is what the position
/// is built from, because a Token-2022 transfer fee delivers less than was
/// sent. A zero delta therefore has to be caught explicitly: the transfer of
/// zero itself succeeds, and everything downstream would otherwise build a
/// position on no collateral at all.
#[test]
fn a_zero_collateral_deposit_is_refused() {
    let mut fixture = Fixture::new(active_params());
    let vault_before = fixture.token_balance(fixture.quote_vault);
    let (newcomer, tokens) = fixture.newcomer();
    let instruction = fixture.open_ix_for(
        newcomer.pubkey(),
        tokens,
        SIDE_LONG,
        BIG_SIZE,
        0,
        unbounded_limit(SIDE_LONG),
    );
    expect_error(
        fixture.send(instruction, &[&newcomer]),
        PerpsError::ZeroAmount,
    );
    assert_eq!(fixture.token_balance(fixture.quote_vault), vault_before);
    fixture.assert_i1("after the refused open");
}

/// Listing a market is refused while creation is paused, and refused again
/// while the feed is revoked.
///
/// Two different mechanisms with the same consequence: the pause is a handler
/// check on the exchange's flags, revocation is a constraint on the `feed`
/// account. Qualifying the feed in between shows the pause is scoped to the one
/// instruction it names.
#[test]
fn listing_a_market_is_refused_while_paused_or_revoked() {
    let mut fixture = Fixture::new(active_params());
    let price_update = fixture.other_price_update(OTHER_FEED_ID);
    fixture.write_price(price_update, OTHER_FEED_ID, 10_000);

    fixture.set_pause_flags(PauseFlags::CREATE_MARKET);
    fixture
        .qualify_feed(OTHER_FEED_ID)
        .expect("qualifying a feed carries no pause gate");
    expect_error(
        fixture.create_market(OTHER_FEED_ID),
        PerpsError::MarketCreationPaused,
    );

    fixture.set_pause_flags(0);
    let other_feed = pda(&[b"feed", OTHER_FEED_ID.as_ref()]);
    fixture
        .set_feed_revoked_for(other_feed, true)
        .expect("revoke the feed");
    expect_error(
        fixture.create_market(OTHER_FEED_ID),
        PerpsError::FeedRevoked,
    );

    // The positive control, and the count that proves it landed.
    assert_eq!(fixture.exchange_state().num_markets, 1);
    fixture
        .set_feed_revoked_for(other_feed, false)
        .expect("restore the feed");
    fixture
        .create_market(OTHER_FEED_ID)
        .expect("with neither gate closed the market lists");
    assert_eq!(fixture.exchange_state().num_markets, 2);
}

/// Spec §9.11, answered with a number instead of an argument.
///
/// The M5 spec says: "Nothing in this document establishes it fits in one
/// transaction… Measure it in stage 3 before the instruction set is frozen."
/// Stage 3 shipped without measuring and M5 is now deployed, so this is overdue.
///
/// The bound asserted is Solana's DEFAULT per-instruction limit of 200,000 CU —
/// what a transaction gets when it carries no ComputeBudget instruction.
/// Exceeding it is not fatal on its own, since a caller may request up to
/// 1,400,000; it means EVERY caller must request more, and one that forgets
/// fails at runtime rather than at build time. Asserting the default is what
/// makes that a build-time fact.
///
/// Each row prints as it is measured, deliberately. The first version of this
/// test collected every number and printed the table at the end, so when the
/// last step failed it reported nothing at all — four measurements that had
/// already succeeded were thrown away with the panic. A measurement that only
/// survives the happy path is not a measurement.
///
/// Integer percentages on purpose: floats are banned here, and the guardrail
/// scanning only `*/src` today is not something a test should lean on.
#[test]
fn the_position_lifecycle_fits_the_default_compute_budget() {
    const DEFAULT_CU_LIMIT: u64 = 200_000;

    fn record(rows: &mut Vec<(&'static str, u64)>, name: &'static str, cu: u64) {
        let pct = cu * 100 / DEFAULT_CU_LIMIT;
        println!("  {name:<26} {cu:>7} CU  {pct:>3}% of the default limit");
        rows.push((name, cu));
    }

    println!("\ncompute units consumed (default per-instruction limit {DEFAULT_CU_LIMIT}):");
    let mut rows: Vec<(&'static str, u64)> = Vec::new();

    let mut fixture = Fixture::new(active_params());
    let trader = fixture.trader.insecure_clone();
    let stranger = fixture.lp.insecure_clone();

    let ix = fixture.open_ix(SIDE_LONG, BIG_SIZE, 1_100 * ONE);
    let cu = fixture
        .send_cu(ix, &[&trader])
        .expect("open must succeed to be measured");
    record(&mut rows, "open_position", cu);

    let ix = fixture.refresh_market_price_ix();
    let cu = fixture
        .send_cu(ix, &[&stranger])
        .expect("refresh must succeed to be measured");
    record(&mut rows, "refresh_market_price", cu);

    let ix = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::SettleMarket {
            pool: fixture.pool,
            market: fixture.market,
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::SettleMarket {}.data(),
    };
    let cu = fixture
        .send_cu(ix, &[&stranger])
        .expect("settle must succeed to be measured");
    record(&mut rows, "settle_market", cu);

    // The one §9.11 names as the risk: an oracle read, funding and borrow
    // settlement, fee maths and a token-transfer CPI in a single instruction.
    let ix = fixture.close_ix();
    let cu = fixture
        .send_cu(ix, &[&trader])
        .expect("close must succeed to be measured");
    record(&mut rows, "close_position", cu);

    // The emergency path consumes a position and is refused unless the market is
    // quarantined and the delay has elapsed, so it needs its own fixture and the
    // same preconditions the emergency tests use.
    let mut fresh = Fixture::new(active_params());
    fresh
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open for the emergency path");
    fresh
        .set_risk_params(fresh.market, quarantined_params(active_params()))
        .expect("quarantine");
    fresh.advance(EMERGENCY_CLOSE_DELAY_SECONDS + 1, 10);
    let admin = fresh.admin.insecure_clone();
    let ix = fresh.emergency_close_ix();
    let cu = fresh
        .send_cu(ix, &[&admin])
        .expect("emergency close must succeed to be measured");
    record(&mut rows, "emergency_close_position", cu);

    let over: Vec<(&str, u64)> = rows
        .iter()
        .copied()
        .filter(|(_, cu)| *cu >= DEFAULT_CU_LIMIT)
        .collect();
    assert!(
        over.is_empty(),
        "these do not fit the default compute budget, so every caller would have to send \
         an explicit ComputeBudget request or fail at runtime: {over:?}"
    );
}

// ── permissionless liquidation ──────────────────────────────────────────────

/// A stranger, funded and holding an empty collateral account.
fn make_keeper(fixture: &mut Fixture) -> (Keypair, Address) {
    let keeper = Keypair::new();
    fixture
        .svm
        .airdrop(&keeper.pubkey(), 100 * 1_000_000_000)
        .unwrap();
    let account = funded_token_account(
        &mut fixture.svm,
        &fixture.collateral_mint,
        &fixture.admin,
        &keeper,
        0,
    );
    (keeper, account)
}

impl Fixture {
    fn liquidate_ix_with(
        &self,
        keeper: &Keypair,
        keeper_token_account: Address,
        owner_token_account: Address,
    ) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::LiquidatePosition {
                exchange: self.exchange,
                keeper: keeper.pubkey(),
                pool: self.pool,
                market: self.market,
                price_update: self.price_update,
                owner: self.trader.pubkey(),
                position: self.position_key(),
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                owner_token_account,
                keeper_token_account,
                token_program: spl_token_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::LiquidatePosition {}.data(),
        }
    }

    fn liquidate(
        &mut self,
        keeper: &Keypair,
        keeper_token_account: Address,
    ) -> Result<Vec<String>, TransactionError> {
        let ix = self.liquidate_ix_with(keeper, keeper_token_account, self.trader_token_account);
        let signer = keeper.insecure_clone();
        self.send_meta(ix, &[&signer])
    }
}

/// The instruction §9.4 asks for: a stranger closes an underwater position and
/// is paid for it.
///
/// The price is the same one the late-liquidation test uses — down 4.5%, equity
/// decayed to $50 against $500 of collateral, with the fee computed on $9,550 of
/// current notional and therefore clamped. That case is chosen deliberately: it
/// is the *ordinary* one once liquidation is permissionless, and it is where a
/// keeper share taken from an unclamped fee would overdraw the vault.
#[test]
fn anyone_can_liquidate_an_underwater_position_and_is_paid_for_it() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 550 * ONE)
        .expect("open a position that can go underwater");
    fixture.set_price(9_550);

    let (keeper, keeper_token_account) = make_keeper(&mut fixture);
    let keeper_before = fixture.token_balance(keeper_token_account);
    let trader_before = fixture.token_balance(fixture.trader_token_account);
    assert_eq!(keeper_before, 0, "the keeper starts with nothing");

    let logs = fixture
        .liquidate(&keeper, keeper_token_account)
        .expect("a stranger must be able to liquidate an underwater position");
    let closed: PositionClosed = event(&logs);

    assert_eq!(
        closed.reason,
        CloseReason::Liquidated,
        "the permissionless path must be distinguishable from the admin one"
    );

    // Non-vacuity. A liquidation whose fee clamped to zero would pay the keeper
    // nothing and this test would pass while proving nothing at all.
    assert!(
        closed.liquidation_fee_quote > 0,
        "the liquidation fee must be non-zero or the keeper assertion is vacuous"
    );

    let expected_keeper =
        closed.liquidation_fee_quote * u64::from(KEEPER_FEE_SHARE_BPS) / u64::from(BPS_DENOMINATOR);
    assert!(expected_keeper > 0, "the keeper's share must be non-zero");
    assert_eq!(
        fixture.token_balance(keeper_token_account) - keeper_before,
        expected_keeper,
        "the keeper is paid its floored share of the clamped liquidation fee"
    );

    // The trader still receives the settlement. The keeper's cut comes out of the
    // fee, not out of what the position was owed.
    assert_eq!(
        fixture.token_balance(fixture.trader_token_account) - trader_before,
        closed.net_payout_quote,
        "the trader receives the net payout, undiminished by the keeper's fee"
    );

    fixture.assert_i1("after a permissionless liquidation");
}

/// The safety property of the whole instruction.
///
/// If a solvent position could be closed by a stranger, "permissionless
/// liquidation" would just be permissionless confiscation. The gate is the
/// position's own numbers, so an unmoved price must refuse every caller.
#[test]
fn a_solvent_position_cannot_be_liquidated_by_a_stranger() {
    let mut fixture = Fixture::new(active_params());
    fixture
        .open(SIDE_LONG, BIG_SIZE, 1_100 * ONE)
        .expect("open a healthy position");

    let (keeper, keeper_token_account) = make_keeper(&mut fixture);
    expect_error(
        fixture.liquidate(&keeper, keeper_token_account).map(|_| ()),
        PerpsError::PositionNotLiquidatable,
    );

    // Still open, still the trader's.
    assert_eq!(
        fixture.position_state().owner,
        fixture.trader.pubkey(),
        "a refused liquidation must leave the position untouched"
    );
}

/// The constraint that separates a liquidation from a theft.
///
/// On the admin path this stops a trusted party redirecting a payout. Here the
/// caller is an arbitrary stranger, so without it the instruction would pay the
/// caller the trader's remaining collateral — and the trader would be left with
/// the position's rent.
#[test]
fn a_liquidator_cannot_redirect_the_traders_payout_to_itself() {
    let mut fixture = Fixture::new(active_params());
    fixture.open(SIDE_LONG, BIG_SIZE, 550 * ONE).expect("open");
    fixture.set_price(9_550);

    let (keeper, keeper_token_account) = make_keeper(&mut fixture);

    // Name the keeper's own account as the trader's payout destination.
    let ix = fixture.liquidate_ix_with(&keeper, keeper_token_account, keeper_token_account);
    let signer = keeper.insecure_clone();
    expect_error(
        fixture.send_meta(ix, &[&signer]).map(|_| ()),
        PerpsError::NotTokenOwner,
    );
}

/// A keeper cannot direct its fee into an account it does not own.
///
/// Without this the fee destination is an arbitrary token account, which makes
/// the payout a griefing tool rather than an incentive.
#[test]
fn a_keeper_cannot_send_its_fee_to_an_account_it_does_not_own() {
    let mut fixture = Fixture::new(active_params());
    fixture.open(SIDE_LONG, BIG_SIZE, 550 * ONE).expect("open");
    fixture.set_price(9_550);

    let (keeper, _keeper_token_account) = make_keeper(&mut fixture);
    let trader_account = fixture.trader_token_account;

    // The trader's account is a valid collateral account — it just is not the
    // keeper's.
    let ix = fixture.liquidate_ix_with(&keeper, trader_account, trader_account);
    let signer = keeper.insecure_clone();
    expect_error(
        fixture.send_meta(ix, &[&signer]).map(|_| ()),
        PerpsError::NotKeeperTokenOwner,
    );
}

/// Permissionless does not mean ungoverned: the same pause flag stops it.
///
/// `LIQUIDATE` rather than `CLOSE_POSITION`, matching the admin path — pausing
/// forced exits is one decision, and it should not depend on who is forcing.
#[test]
fn a_paused_exchange_stops_keepers_as_well_as_the_admin() {
    let mut fixture = Fixture::new(active_params());
    fixture.open(SIDE_LONG, BIG_SIZE, 550 * ONE).expect("open");
    fixture.set_price(9_550);
    fixture.set_pause_flags(PauseFlags::LIQUIDATE);

    let (keeper, keeper_token_account) = make_keeper(&mut fixture);
    expect_error(
        fixture.liquidate(&keeper, keeper_token_account).map(|_| ()),
        PerpsError::LiquidationPaused,
    );
}
