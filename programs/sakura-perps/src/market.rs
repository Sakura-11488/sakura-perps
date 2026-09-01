//! Qualified feeds and markets.
//!
//! Two accounts, deliberately separated by who may write them.
//!
//! A [`QualifiedFeed`] is the admin's statement that a particular Pyth feed is
//! safe to price against, and it carries **every** risk-bearing oracle
//! parameter. A [`Market`] is then listed permissionlessly against a qualified
//! feed, copying those parameters rather than choosing any of them. That split
//! is the whole safety argument for permissionless listing: whoever creates a
//! market picks the feed from an allowlist and nothing else.
//!
//! A market is born quarantined — every risk parameter is zero, and
//! `max_oi_usd == 0` means no position can be opened. Only an admin calling
//! `set_risk_params` lifts that, so a market that exists is not yet a market
//! that trades.
//!
//! # Accrual is continuous and unpausable
//!
//! [`accrue`] is the one implementation of borrow and funding accrual, called by
//! the standalone `settle_market` instruction and at the head of all four
//! position instructions. It reads no oracle, so it still runs when the feed is
//! degraded — which is when it matters most — and it has no pause gate, because
//! a pause that stops the clock is a subsidy to whichever side is paying.
//!
//! # The divergence clamp is not a rejection
//!
//! [`clamp_to_ema_band`] pulls an exit price into the feed's own EMA band in
//! **both** directions. Rejecting instead would be a trap most valuable to a
//! manipulator at exactly the moment it fired, and an adverse-only clamp would
//! stop the pool paying out on a manipulated price while doing nothing to stop
//! it charging on one.

use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;
use sakura_perps_risk::funding::{
    borrow_index_delta, fees_dominate_funding, funding_index_delta, funding_rate_per_hour,
};
use sakura_perps_risk::math::{mul_div_ceil, mul_div_floor};
use sakura_perps_risk::oracle::{
    validate_guard_ordering, OracleGuards, ValidatedPrice, MAX_EXPONENT, MIN_EXPONENT,
};
use sakura_perps_risk::pool::utilization_bps;
use sakura_perps_risk::position::validate_margin_parameters;
use sakura_perps_risk::scale::RATE_SCALE;

use crate::{Exchange, PauseFlags, PerpsError, Pool};

/// Upper bound on a market's spread, in basis points.
///
/// A spread is charged against the trader on both legs; leaving it unbounded
/// would let an admin make a market that cannot be traded profitably at any
/// price while still appearing open.
pub const MAX_SPREAD_BPS: u16 = 500;

/// Upper bound on either trade fee, in basis points.
pub const MAX_TRADE_FEE_BPS: u16 = 500;

/// Upper bound on a feed's `asset_decimals`.
///
/// Not a tidiness bound. Notional divides by `10^asset_decimals`, and
/// `sakura_perps_risk::math::pow10` is defined only to `10^38`, so a value past
/// this turns **every** notional in the market into a `MathOverflow` — a market
/// that lists, activates, and then cannot price a single trade.
pub const MAX_ASSET_DECIMALS: u8 = 18;

/// The holding period the fee-versus-funding check is stated over.
///
/// A separate constant rather than `min_settle_interval_seconds`, which is a
/// rate-**resample** interval and means something else entirely. What this
/// asserts is exactly what it says: a round trip's fees exceed the funding
/// accruable in one hour. **The claim that this makes funding-farming
/// unprofitable is withdrawn** — a multi-hour farm is not addressed by it, and
/// that gap is recorded as open rather than papered over.
pub const POLICY_HOLDING_PERIOD_SECONDS: u64 = 3_600;

/// Upper bound on `max_settle_window_seconds`: seven days.
///
/// The window caps Δt per accrual call. A longer one lets a market nobody
/// settled for a month charge the whole month in a single jump, against a
/// utilisation figure that has nothing to do with the period being charged for.
pub const MAX_SETTLE_WINDOW_SECONDS: u32 = 7 * 24 * 60 * 60;

/// One percent per hour, the ceiling on `borrow_rate_per_hour`.
///
/// The ceiling is what makes `cum_borrow_index` safe, and the arithmetic is
/// written out because the unbounded version was a blocker. `borrow_index_delta`
/// accrues at most `rate × 10_000 / (10_000 × 3_600)` per second — about 2 778
/// index units per second at full utilisation — so a century of continuous
/// accrual reaches roughly `8.8e12`. `borrow_owed` is `notional × Δindex /
/// RATE_SCALE` with notional bounded by `max_oi_usd`, which keeps the product
/// many orders below `i128::MAX`, the bound `equity` actually needs.
///
/// Unbounded, an admin could set `9e30`, at which roughly nineteen hours of
/// one-second settles pushes `borrow_owed` past `i128::MAX` and **every** close
/// in the market reverts.
pub const MAX_BORROW_RATE_PER_HOUR: u128 = RATE_SCALE / 100;

/// One percent per hour, the ceiling on `funding_cap_per_hour`. See
/// [`MAX_BORROW_RATE_PER_HOUR`] — the same argument, on the funding index.
pub const MAX_FUNDING_RATE_PER_HOUR: u128 = RATE_SCALE / 100;

/// Ceiling on `funding_sensitivity`.
///
/// Sensitivity multiplies the open-interest skew before the cap applies, so it
/// cannot move the rate past `funding_cap_per_hour`. It is bounded anyway, so
/// the intermediate product stays small enough to reason about.
pub const MAX_FUNDING_SENSITIVITY: u128 = RATE_SCALE;

/// How many dollars of the pool's reserve budget one dollar of trader
/// collateral may consume.
///
/// `reserve_quote` scales with notional while the trader posts only
/// `initial_margin_bps` of it, so a dollar of collateral consumes
/// `max_profit_bps / initial_margin_bps` dollars of a pool-global budget. At the
/// parameters this protocol was first configured with, that ratio was twenty.
pub const MAX_RESERVE_LEVERAGE: u16 = 4;

/// How long a market must have been quarantined before positions in it can be
/// wound down without an oracle.
///
/// Both of `emergency_close_position`'s preconditions are public and slow, so a
/// wind-down is announced by the chain a day before any value moves.
pub const EMERGENCY_CLOSE_DELAY_SECONDS: i64 = 86_400;

/// An oracle feed the admin has declared safe to price a market against.
///
/// Seeds `[b"feed", feed_id]`. This is the allowlist the protocol's core safety
/// claim rests on: `oracle::load_price` will accept no other feed id, and
/// `price_update` pins the exact account those prices must come from.
#[account]
#[derive(InitSpace)]
pub struct QualifiedFeed {
    pub bump: u8,
    /// Pyth feed id — the only value `load_price` may be given for this market.
    pub feed_id: [u8; 32],
    /// The pinned `PriceUpdateV2` account.
    ///
    /// Without this, a caller supplies their own price account and the feed id
    /// check alone does not save you: it proves the *message* is for the right
    /// feed, not that the account was written by anyone trustworthy.
    pub price_update: Pubkey,
    /// Exponent recorded at qualification. A feed that rescales silently
    /// multiplies every price by a power of ten.
    pub expected_exponent: i32,
    /// Base-unit decimals of the traded asset.
    pub asset_decimals: u8,
    /// Sanity band, at `PRICE_SCALE`.
    pub min_price: u128,
    /// See [`QualifiedFeed::min_price`].
    pub max_price: u128,

    // ── Trading guards: tight. A trade that cannot be priced confidently
    //    simply does not happen. ──────────────────────────────────────────────
    pub trading_max_age_seconds: u32,
    pub trading_max_age_slots: u64,
    pub trading_max_future_skew_seconds: u32,
    pub trading_max_confidence_bps: u16,

    // ── Liquidation guards: deliberately looser. Refusing to liquidate is not
    //    a safe default — the position keeps losing and the pool absorbs it. ──
    pub liquidation_max_age_seconds: u32,
    pub liquidation_max_age_slots: u64,
    pub liquidation_max_future_skew_seconds: u32,
    pub liquidation_max_confidence_bps: u16,

    /// Spot-versus-EMA gate applied when opening.
    pub max_divergence_bps: u16,
    /// Revoked feeds are flagged, never closed.
    ///
    /// A close would let the same seeds be re-initialised with different
    /// parameters, so re-qualification could silently become a fresh start for
    /// markets already trading against it.
    pub revoked: bool,
    pub _reserved: [u8; 64],
}

impl QualifiedFeed {
    /// Guards for opening a position.
    pub fn trading_guards(&self) -> OracleGuards {
        OracleGuards {
            max_age_seconds: self.trading_max_age_seconds,
            max_age_slots: self.trading_max_age_slots,
            max_future_skew_seconds: self.trading_max_future_skew_seconds,
            max_confidence_bps: self.trading_max_confidence_bps,
            min_price: self.min_price,
            max_price: self.max_price,
            expected_exponent: self.expected_exponent,
        }
    }

    /// Guards for liquidating one.
    pub fn liquidation_guards(&self) -> OracleGuards {
        OracleGuards {
            max_age_seconds: self.liquidation_max_age_seconds,
            max_age_slots: self.liquidation_max_age_slots,
            max_future_skew_seconds: self.liquidation_max_future_skew_seconds,
            max_confidence_bps: self.liquidation_max_confidence_bps,
            min_price: self.min_price,
            max_price: self.max_price,
            expected_exponent: self.expected_exponent,
        }
    }
}

/// A tradeable market, listed against a [`QualifiedFeed`].
///
/// Seeds `[b"market", feed_id]` — one market per feed, structurally. A second
/// `create_market` for the same feed fails at account creation rather than
/// needing a check.
#[account]
#[derive(InitSpace)]
pub struct Market {
    pub bump: u8,
    /// Index assigned from `exchange.num_markets` at creation.
    pub market_index: u32,

    // ── Copied from the qualified feed at creation, and tightenable only ─────
    pub feed_id: [u8; 32],
    pub price_update: Pubkey,
    pub expected_exponent: i32,
    pub asset_decimals: u8,
    pub min_price: u128,
    pub max_price: u128,
    pub trading_max_age_seconds: u32,
    pub trading_max_age_slots: u64,
    pub trading_max_future_skew_seconds: u32,
    pub trading_max_confidence_bps: u16,
    pub liquidation_max_age_seconds: u32,
    pub liquidation_max_age_slots: u64,
    pub liquidation_max_future_skew_seconds: u32,
    pub liquidation_max_confidence_bps: u16,
    pub max_divergence_bps: u16,

    // ── Risk parameters. All zero at creation; `max_oi_usd == 0` IS the
    //    quarantine, so a freshly listed market cannot be traded. ─────────────
    pub initial_margin_bps: u16,
    pub maintenance_margin_bps: u16,
    pub liquidation_fee_bps: u16,
    /// Profit cap, as a fraction of **entry notional** — not of collateral.
    pub max_profit_bps: u16,
    pub spread_bps: u16,
    pub open_fee_bps: u16,
    pub close_fee_bps: u16,
    /// Per-side open-interest cap. Zero means quarantined.
    pub max_oi_usd: u128,
    /// Largest move the asset makes inside `trading_max_age_seconds`, used to
    /// price the staleness risk a trader is taking rather than only gate it.
    pub max_oracle_drift_bps: u16,
    pub min_position_size_base: u64,
    pub min_notional_usd: u128,
    pub min_collateral_usd: u128,

    // ── Funding and borrow ──────────────────────────────────────────────────
    pub borrow_rate_per_hour: u128,
    pub funding_sensitivity: u128,
    pub funding_cap_per_hour: u128,
    /// Caps Δt per settle call, so a long gap cannot accrue in one jump.
    pub max_settle_window_seconds: u32,
    /// Rate-resample interval. **Not** a floor on accrual: accrual is
    /// continuous, and throttling it would make what is owed depend on how
    /// often somebody called settle.
    pub min_settle_interval_seconds: u32,
    pub cum_borrow_index: u128,
    /// The undivided remainder from borrow accrual.
    ///
    /// Carried because flooring per call made 3600 one-second settles accrue
    /// exactly zero where one 3600-second settle accrued 3590 — borrow revenue
    /// evaporating as a function of settle cadence.
    pub borrow_remainder_carry: u128,
    pub cum_funding_index: i128,
    /// Last sampled rate, applied across the whole interval.
    pub sampled_funding_rate_per_hour: i128,
    pub last_settle_ts: i64,
    pub last_rate_sample_ts: i64,

    // ── Book ────────────────────────────────────────────────────────────────
    /// Open interest at **entry** notional, matching what funding is charged on.
    pub long_oi_usd: u128,
    /// See [`Market::long_oi_usd`].
    pub short_oi_usd: u128,
    pub long_positions: u32,
    pub short_positions: u32,
    /// This market's slice of `pool.locked_quote`.
    pub locked_quote: u64,
    /// This market's slice of `pool.reserved_quote`.
    pub reserved_quote: u64,
    /// Bad debt recorded here, never socialised in M5.
    pub cum_bad_debt_usd: u128,

    /// When the market was last quarantined, and the clock
    /// `emergency_close_position`'s delay runs from.
    ///
    /// Written **only on the transition** in and out of quarantine. A retune
    /// that does not cross the boundary must leave it alone, or every fee change
    /// would restart the wind-down delay.
    pub quarantined_ts: i64,
    /// The oracle-free settlement reference: the last price this market
    /// transacted at, clamped into its own EMA band.
    ///
    /// Written by every successful guard-passing price read — both trading
    /// paths, the admin settlement, and the permissionless
    /// `refresh_market_price`. It is read out of this account rather than from
    /// an oracle, which is what lets `emergency_close_position` take no price
    /// account at all.
    pub last_good_price: u128,
    /// When [`Market::last_good_price`] was written. Observability only; it
    /// gates nothing, deliberately — a freshness gate on it would hand the
    /// oracle back the veto the field exists to remove.
    pub last_good_price_ts: i64,

    /// 128 originally. `quarantined_ts`, `last_good_price` and
    /// `last_good_price_ts` were taken from here rather than appended, so the
    /// account's length is unchanged and no existing instance needs
    /// reallocating. A market written before those fields existed reads zero for
    /// all three, which is the "never quarantined, never priced" case both the
    /// wind-down delay and `emergency_reference_price` already handle.
    pub _reserved: [u8; 96],
}

// The on-chain length is frozen. Anchor has no migration story: a changed
// `INIT_SPACE` orphans every allocated instance rather than growing it. Every
// stage-3 field came out of `_reserved`, and these are the checks that say so at
// compile time rather than in a comment.
const _: () = assert!(Market::INIT_SPACE == 552);
const _: () = assert!(QualifiedFeed::INIT_SPACE == 205);

impl Market {
    /// Whether the market is still quarantined — listed, but not tradeable.
    ///
    /// A market is created with every risk parameter at zero and only an admin
    /// can lift that. Reading the quarantine off `max_oi_usd` rather than a
    /// separate flag means there is no state where the flag says tradeable and
    /// the parameters are still zero.
    pub fn is_quarantined(&self) -> bool {
        self.max_oi_usd == 0
    }

    /// Guards for opening a position.
    pub fn trading_guards(&self) -> OracleGuards {
        OracleGuards {
            max_age_seconds: self.trading_max_age_seconds,
            max_age_slots: self.trading_max_age_slots,
            max_future_skew_seconds: self.trading_max_future_skew_seconds,
            max_confidence_bps: self.trading_max_confidence_bps,
            min_price: self.min_price,
            max_price: self.max_price,
            expected_exponent: self.expected_exponent,
        }
    }

    /// Guards for liquidating one.
    pub fn liquidation_guards(&self) -> OracleGuards {
        OracleGuards {
            max_age_seconds: self.liquidation_max_age_seconds,
            max_age_slots: self.liquidation_max_age_slots,
            max_future_skew_seconds: self.liquidation_max_future_skew_seconds,
            max_confidence_bps: self.liquidation_max_confidence_bps,
            min_price: self.min_price,
            max_price: self.max_price,
            expected_exponent: self.expected_exponent,
        }
    }
}

/// Arguments to [`crate::sakura_perps::qualify_feed`].
///
/// The fourteen values `create_market` will copy by value. `price_update` is not
/// among them: it comes from the account passed alongside, so the binding is to
/// an account that exists rather than to a pubkey somebody typed.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct QualifyFeedParams {
    pub feed_id: [u8; 32],
    pub expected_exponent: i32,
    pub asset_decimals: u8,
    pub min_price: u128,
    pub max_price: u128,
    pub trading_max_age_seconds: u32,
    pub trading_max_age_slots: u64,
    pub trading_max_future_skew_seconds: u32,
    pub trading_max_confidence_bps: u16,
    pub liquidation_max_age_seconds: u32,
    pub liquidation_max_age_slots: u64,
    pub liquidation_max_future_skew_seconds: u32,
    pub liquidation_max_confidence_bps: u16,
    pub max_divergence_bps: u16,
}

pub fn handle_qualify_feed(ctx: Context<QualifyFeed>, params: QualifyFeedParams) -> Result<()> {
    // 1. The exponent must be one `normalize_price` can actually apply. Outside
    //    this range every price in the market is `UnexpectedExponent`.
    require!(
        (MIN_EXPONENT..=MAX_EXPONENT).contains(&params.expected_exponent),
        PerpsError::InvalidFeedParameters
    );

    // 2. A sanity band that is not a band gates nothing.
    require!(
        params.min_price > 0
            && params.min_price < params.max_price
            && params.asset_decimals <= MAX_ASSET_DECIMALS,
        PerpsError::InvalidFeedParameters
    );

    // 3. A 100% divergence tolerance is not a tolerance, and the exit clamp
    //    needs the strict inequality: at `BPS_DENOMINATOR` the lower edge of the
    //    band is zero, and a mid clamped to zero is unvaluable.
    require!(
        params.max_divergence_bps > 0 && params.max_divergence_bps < crate::BPS_DENOMINATOR,
        PerpsError::InvalidFeedParameters
    );

    // 6. **Totality of `execution_price`.** `validate_price` admits a price only
    //    when `confidence × 10_000 <= max_confidence_bps × mid`, and
    //    `execution_price` refuses to strike one when `confidence + mid ×
    //    spread_bps / 10_000 >= mid`. Composing the two, the adverse adjustment
    //    is at most `mid × (max_confidence_bps + spread_bps) / 10_000`, which is
    //    strictly below `mid` **iff** the two bps figures sum below 10 000.
    //
    //    Checked against the **liquidation** gate and `MAX_SPREAD_BPS`, which
    //    covers both guard sets and every legal spread at once: guard ordering
    //    permits the liquidation confidence gate to be the wider of the two, and
    //    `admin_settle_position` prices under it. Get this wrong and the failure
    //    is not a bad price — it is an exit that cannot be priced at all.
    //
    //    The exit paths clamp the mid into the feed's EMA band, which would
    //    break this composition if the confidence stayed scaled to the
    //    unclamped spot. It does not: `clamp_to_ema_band` rescales it by the
    //    same factor, so the inequality holds against the number actually
    //    passed to `execution_price`.
    require!(
        u32::from(params.liquidation_max_confidence_bps) + u32::from(MAX_SPREAD_BPS)
            < u32::from(crate::BPS_DENOMINATOR),
        PerpsError::ConfidenceGateTooWide
    );

    let clock = Clock::get()?;

    // The account is written before validations 4 and 5 run, deliberately. Those
    // two read the guards back through `feed.trading_guards()` and
    // `feed.liquidation_guards()` — the same two methods every later instruction
    // calls — rather than through a parallel construction of the same fourteen
    // numbers that could drift from them. A failed `require!` below reverts the
    // account creation along with everything else, so nothing invalid survives.
    let feed = &mut ctx.accounts.feed;
    feed.bump = ctx.bumps.feed;
    feed.feed_id = params.feed_id;
    feed.price_update = ctx.accounts.price_update.key();
    feed.expected_exponent = params.expected_exponent;
    feed.asset_decimals = params.asset_decimals;
    feed.min_price = params.min_price;
    feed.max_price = params.max_price;
    feed.trading_max_age_seconds = params.trading_max_age_seconds;
    feed.trading_max_age_slots = params.trading_max_age_slots;
    feed.trading_max_future_skew_seconds = params.trading_max_future_skew_seconds;
    feed.trading_max_confidence_bps = params.trading_max_confidence_bps;
    feed.liquidation_max_age_seconds = params.liquidation_max_age_seconds;
    feed.liquidation_max_age_slots = params.liquidation_max_age_slots;
    feed.liquidation_max_future_skew_seconds = params.liquidation_max_future_skew_seconds;
    feed.liquidation_max_confidence_bps = params.liquidation_max_confidence_bps;
    feed.max_divergence_bps = params.max_divergence_bps;
    // Re-qualification is deliberately unsupported: `init` fails on an existing
    // feed, and the alternative — mutating a feed markets have already copied —
    // would let a position be settled under numbers it was never opened under.
    feed.revoked = false;

    // 4. Liquidation guards must be at least as permissive as trading guards on
    //    every axis, or positions become openable at prices they cannot be
    //    liquidated at.
    validate_guard_ordering(&feed.trading_guards(), &feed.liquidation_guards())
        .map_err(crate::oracle::map_risk_error)?;

    // 5. The passed account must answer *today*: right feed id, right exponent,
    //    inside the band, fresh on both clocks, confidence inside the trading
    //    gate. Qualifying against an account that does not is a configuration
    //    error, and it is free to catch here rather than at the first trade.
    crate::oracle::load_price(
        &ctx.accounts.price_update,
        &feed.feed_id,
        &feed.trading_guards(),
        &clock,
    )?;

    emit!(FeedQualified {
        feed: feed.key(),
        feed_id: feed.feed_id,
        price_update: feed.price_update,
        expected_exponent: feed.expected_exponent,
        asset_decimals: feed.asset_decimals,
    });

    Ok(())
}

pub fn handle_set_feed_revoked(ctx: Context<SetFeedRevoked>, revoked: bool) -> Result<()> {
    // One bit, reversible, touching no value. There is nothing to validate that
    // the `address = exchange.admin` constraint has not validated already.
    let feed = &mut ctx.accounts.feed;
    feed.revoked = revoked;

    emit!(FeedRevocationChanged {
        feed: feed.key(),
        feed_id: feed.feed_id,
        revoked,
    });

    Ok(())
}

pub fn handle_create_market(ctx: Context<CreateMarket>) -> Result<()> {
    require!(
        ctx.accounts.exchange.paused_flags & PauseFlags::CREATE_MARKET == 0,
        PerpsError::MarketCreationPaused
    );

    let clock = Clock::get()?;
    let feed = &ctx.accounts.feed;
    let market_index = ctx.accounts.exchange.num_markets;

    let market = &mut ctx.accounts.market;
    market.bump = ctx.bumps.market;
    market.market_index = market_index;

    // Fifteen fields copied **by value**. Reading them through the feed at trade
    // time would mean revoking or re-tuning a feed silently re-priced every
    // market listed against it.
    market.feed_id = feed.feed_id;
    market.price_update = feed.price_update;
    market.expected_exponent = feed.expected_exponent;
    market.asset_decimals = feed.asset_decimals;
    market.min_price = feed.min_price;
    market.max_price = feed.max_price;
    market.trading_max_age_seconds = feed.trading_max_age_seconds;
    market.trading_max_age_slots = feed.trading_max_age_slots;
    market.trading_max_future_skew_seconds = feed.trading_max_future_skew_seconds;
    market.trading_max_confidence_bps = feed.trading_max_confidence_bps;
    market.liquidation_max_age_seconds = feed.liquidation_max_age_seconds;
    market.liquidation_max_age_slots = feed.liquidation_max_age_slots;
    market.liquidation_max_future_skew_seconds = feed.liquidation_max_future_skew_seconds;
    market.liquidation_max_confidence_bps = feed.liquidation_max_confidence_bps;
    market.max_divergence_bps = feed.max_divergence_bps;

    // Every risk parameter is written as an explicit zero rather than left to
    // Anchor's zeroed account. `max_oi_usd == 0` *is* the quarantine, so this
    // block is the safety property and not initialisation boilerplate.
    market.initial_margin_bps = 0;
    market.maintenance_margin_bps = 0;
    market.liquidation_fee_bps = 0;
    market.max_profit_bps = 0;
    market.spread_bps = 0;
    market.open_fee_bps = 0;
    market.close_fee_bps = 0;
    market.max_oi_usd = 0;
    market.max_oracle_drift_bps = 0;
    market.min_position_size_base = 0;
    market.min_notional_usd = 0;
    market.min_collateral_usd = 0;
    market.borrow_rate_per_hour = 0;
    market.funding_sensitivity = 0;
    market.funding_cap_per_hour = 0;
    market.max_settle_window_seconds = 0;
    market.min_settle_interval_seconds = 0;

    market.cum_borrow_index = 0;
    market.borrow_remainder_carry = 0;
    market.cum_funding_index = 0;
    market.sampled_funding_rate_per_hour = 0;
    // Both clocks start now, so the first accrual measures from creation rather
    // than from the epoch — which would be one jump of fifty-odd years, clamped
    // to the settle window but still charged in full.
    market.last_settle_ts = clock.unix_timestamp;
    market.last_rate_sample_ts = clock.unix_timestamp;

    market.long_oi_usd = 0;
    market.short_oi_usd = 0;
    market.long_positions = 0;
    market.short_positions = 0;
    market.locked_quote = 0;
    market.reserved_quote = 0;
    market.cum_bad_debt_usd = 0;

    // Born quarantined, and the clock for winding it down starts here. A market
    // nobody ever activates becomes emergency-closable a day after creation,
    // which costs nothing: it holds no positions.
    market.quarantined_ts = clock.unix_timestamp;
    market.last_good_price = 0;
    market.last_good_price_ts = 0;

    let market_key = market.key();
    let feed_id = market.feed_id;
    let price_update = market.price_update;

    ctx.accounts.exchange.num_markets = market_index
        .checked_add(1)
        .ok_or(PerpsError::MathOverflow)?;

    emit!(MarketCreated {
        market: market_key,
        feed_id,
        market_index,
        price_update,
    });

    Ok(())
}

/// Arguments to [`crate::sakura_perps::set_risk_params`] — the seventeen values
/// that turn a listed market into a trading one.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct RiskParams {
    pub initial_margin_bps: u16,
    pub maintenance_margin_bps: u16,
    pub liquidation_fee_bps: u16,
    pub max_profit_bps: u16,
    pub spread_bps: u16,
    pub open_fee_bps: u16,
    pub close_fee_bps: u16,
    /// Per-side open-interest cap. **Zero quarantines the market**, and it is
    /// the tightest action an admin has.
    pub max_oi_usd: u128,
    pub max_oracle_drift_bps: u16,
    pub min_position_size_base: u64,
    pub min_notional_usd: u128,
    pub min_collateral_usd: u128,
    pub borrow_rate_per_hour: u128,
    pub funding_sensitivity: u128,
    pub funding_cap_per_hour: u128,
    pub max_settle_window_seconds: u32,
    pub min_settle_interval_seconds: u32,
}

pub fn handle_set_risk_params(ctx: Context<SetRiskParams>, params: RiskParams) -> Result<()> {
    // 1. Initial margin must exceed maintenance plus the liquidation fee, or a
    //    position is liquidatable the moment it opens.
    validate_margin_parameters(
        params.initial_margin_bps,
        params.maintenance_margin_bps,
        params.liquidation_fee_bps,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // 2. Spread and both trade fees inside their ceilings.
    require!(
        params.spread_bps <= MAX_SPREAD_BPS
            && params.open_fee_bps <= MAX_TRADE_FEE_BPS
            && params.close_fee_bps <= MAX_TRADE_FEE_BPS,
        PerpsError::InvalidRiskParameters
    );

    // 3. A zero profit cap reserves nothing and pays nothing; a cap above 100%
    //    of notional reserves more than the position can ever be worth. Zero
    //    initial margin is infinite leverage.
    require!(
        params.max_profit_bps > 0
            && params.max_profit_bps <= crate::BPS_DENOMINATOR
            && params.initial_margin_bps > 0,
        PerpsError::InvalidRiskParameters
    );

    // 4. A round trip's fees must exceed the funding accruable in one hour.
    //    That is the whole of the claim — see [`POLICY_HOLDING_PERIOD_SECONDS`],
    //    which also records what it deliberately does **not** claim.
    require!(
        fees_dominate_funding(
            params.open_fee_bps,
            params.close_fee_bps,
            params.funding_cap_per_hour,
            POLICY_HOLDING_PERIOD_SECONDS,
        )
        .map_err(crate::oracle::map_risk_error)?,
        PerpsError::FeesDoNotDominateFunding
    );

    // 5. A zero settle window clamps every Δt to nothing, so the market never
    //    accrues at all — a different failure from a window that is too long,
    //    and it gets a different variant.
    require!(
        params.max_settle_window_seconds > 0,
        PerpsError::InvalidRiskParameters
    );
    require!(
        params.max_settle_window_seconds <= MAX_SETTLE_WINDOW_SECONDS,
        PerpsError::SettleWindowTooLong
    );

    // 6. Zero minimums make dust positions free to create and expensive to
    //    carry: each one still accrues, still reserves, and still costs an admin
    //    a transaction to settle.
    require!(
        params.min_position_size_base > 0
            && params.min_notional_usd > 0
            && params.min_collateral_usd > 0,
        PerpsError::InvalidRiskParameters
    );

    // 7. Rate ceilings. See [`MAX_BORROW_RATE_PER_HOUR`] for the arithmetic; the
    //    short version is that an unbounded borrow rate makes every close in the
    //    market revert after about nineteen hours.
    require!(
        params.borrow_rate_per_hour <= MAX_BORROW_RATE_PER_HOUR,
        PerpsError::BorrowRateTooHigh
    );
    require!(
        params.funding_cap_per_hour <= MAX_FUNDING_RATE_PER_HOUR,
        PerpsError::FundingRateTooHigh
    );
    require!(
        params.funding_sensitivity <= MAX_FUNDING_SENSITIVITY,
        PerpsError::FundingSensitivityTooHigh
    );

    // 8. Reserve leverage. A dollar of collateral consumes `max_profit_bps /
    //    initial_margin_bps` dollars of a pool-global budget; this bounds that
    //    ratio at [`MAX_RESERVE_LEVERAGE`].
    //
    //    The *per-market* half needs no field and no check: a market's reserve
    //    is `Σ max_profit_bps × entry_notional / 10_000` and open interest is
    //    capped at `max_oi_usd`, so setting `max_oi_usd` **is** setting the
    //    market's reserve budget.
    require!(
        u32::from(params.max_profit_bps)
            <= u32::from(MAX_RESERVE_LEVERAGE) * u32::from(params.initial_margin_bps),
        PerpsError::ReserveLeverageTooHigh
    );

    // 9. The staleness option is charged rather than merely gated. A trader who
    //    sees a price move before the oracle does holds an option worth
    //    `max_oracle_drift_bps` of notional; a round trip costs the two fees plus
    //    the spread on each of two legs. Requiring the second to dominate makes
    //    exercising it unprofitable, and the units line up exactly.
    require!(
        u32::from(params.open_fee_bps)
            + u32::from(params.close_fee_bps)
            + 2 * u32::from(params.spread_bps)
            >= u32::from(params.max_oracle_drift_bps),
        PerpsError::FeesDoNotDominateDrift
    );

    let clock = Clock::get()?;
    let market = &mut ctx.accounts.market;

    // 10. Activation bookkeeping, **on the transition only**. A retune that does
    //     not cross the boundary must leave `quarantined_ts` alone, or the
    //     wind-down delay restarts every time a fee changes — and an admin
    //     tidying up parameters would silently postpone a recovery path.
    let was_quarantined = market.is_quarantined();
    let now_quarantined = params.max_oi_usd == 0;
    if was_quarantined && !now_quarantined {
        market.quarantined_ts = 0;
    } else if !was_quarantined && now_quarantined {
        market.quarantined_ts = clock.unix_timestamp;
    }

    market.initial_margin_bps = params.initial_margin_bps;
    market.maintenance_margin_bps = params.maintenance_margin_bps;
    market.liquidation_fee_bps = params.liquidation_fee_bps;
    market.max_profit_bps = params.max_profit_bps;
    market.spread_bps = params.spread_bps;
    market.open_fee_bps = params.open_fee_bps;
    market.close_fee_bps = params.close_fee_bps;
    market.max_oi_usd = params.max_oi_usd;
    market.max_oracle_drift_bps = params.max_oracle_drift_bps;
    market.min_position_size_base = params.min_position_size_base;
    market.min_notional_usd = params.min_notional_usd;
    market.min_collateral_usd = params.min_collateral_usd;
    market.borrow_rate_per_hour = params.borrow_rate_per_hour;
    market.funding_sensitivity = params.funding_sensitivity;
    market.funding_cap_per_hour = params.funding_cap_per_hour;
    market.max_settle_window_seconds = params.max_settle_window_seconds;
    market.min_settle_interval_seconds = params.min_settle_interval_seconds;

    emit!(RiskParamsSet {
        market: market.key(),
        initial_margin_bps: market.initial_margin_bps,
        maintenance_margin_bps: market.maintenance_margin_bps,
        liquidation_fee_bps: market.liquidation_fee_bps,
        max_profit_bps: market.max_profit_bps,
        spread_bps: market.spread_bps,
        open_fee_bps: market.open_fee_bps,
        close_fee_bps: market.close_fee_bps,
        max_oi_usd: market.max_oi_usd,
        max_oracle_drift_bps: market.max_oracle_drift_bps,
        min_position_size_base: market.min_position_size_base,
        min_notional_usd: market.min_notional_usd,
        min_collateral_usd: market.min_collateral_usd,
        borrow_rate_per_hour: market.borrow_rate_per_hour,
        funding_sensitivity: market.funding_sensitivity,
        funding_cap_per_hour: market.funding_cap_per_hour,
        max_settle_window_seconds: market.max_settle_window_seconds,
        min_settle_interval_seconds: market.min_settle_interval_seconds,
        quarantined: now_quarantined,
        quarantined_ts: market.quarantined_ts,
    });

    Ok(())
}

/// Accrue a market's borrow and funding indices up to `clock`.
///
/// One implementation, six callers: the standalone `settle_market` instruction
/// and the head of `open_position`, `close_position`, `admin_settle_position`,
/// `emergency_close_position` and `liquidate_position`. The standalone
/// instruction exists so accrual does not depend on trading activity; the
/// in-line calls exist so no index is ever *read* stale.
///
/// `liquidate_position` was added and this list was not, which is not a cosmetic
/// slip. Reading "five callers" is what produced a documented claim that an
/// uncranked market leaves a position unliquidatable. It does not: the
/// liquidation accrues before its own solvency gate, so a stale stored index
/// protects nobody. See `a_stale_market_index_does_not_stop_a_keeper_being_paid`.
///
/// Returns the clamped Δt that was accrued, or `None` when nothing was written —
/// which is what lets `settle_market` skip its event rather than emit a no-op
/// one.
///
/// # Δt is clamped at both ends
///
/// `Clock::unix_timestamp` is a stake-weighted vote estimate and is **not**
/// monotonic. A non-positive interval returns early and writes *nothing*: a
/// keeper calling twice in a slot is normal, and a backwards clock revision must
/// never become a ~1.8e19-second accrual through an unguarded `as u64`. That
/// cast is unreachable until positivity is proven, and **no timestamp field in
/// this program is ever written backwards.** The upper clamp is
/// `max_settle_window_seconds`, so a long unattended gap cannot be charged in
/// one jump against a utilisation figure from the far end of it.
pub(crate) fn accrue(market: &mut Market, pool: &Pool, clock: &Clock) -> Result<Option<u64>> {
    let now = clock.unix_timestamp;
    let raw = now
        .checked_sub(market.last_settle_ts)
        .ok_or(PerpsError::MathOverflow)?;
    if raw <= 0 {
        return Ok(None);
    }
    let dt = (raw as u64).min(u64::from(market.max_settle_window_seconds));

    // Borrow accrues against **pool-wide** utilisation, so borrow is coupled
    // across markets: opening a position in one raises the borrow rate in every
    // other. The remainder is persisted rather than discarded, so a market
    // settled every second accrues exactly what one settled hourly does.
    let accrual = borrow_index_delta(
        market.borrow_rate_per_hour,
        utilization_bps(
            u128::from(pool.reserved_quote),
            u128::from(pool.quote_deposited),
        )
        .map_err(crate::oracle::map_risk_error)?,
        dt,
        market.borrow_remainder_carry,
    )
    .map_err(crate::oracle::map_risk_error)?;
    market.cum_borrow_index = market
        .cum_borrow_index
        .checked_add(accrual.index_delta)
        .ok_or(PerpsError::MathOverflow)?;
    market.borrow_remainder_carry = accrual.remainder;

    // Funding accrues at the **last sampled** rate across the whole interval.
    market.cum_funding_index = market
        .cum_funding_index
        .checked_add(
            funding_index_delta(market.sampled_funding_rate_per_hour, dt)
                .map_err(crate::oracle::map_risk_error)?,
        )
        .ok_or(PerpsError::MathOverflow)?;

    // Resample on interval only. Accrual is continuous; only the rate is
    // stepwise. Throttling accrual itself would make what a position owes depend
    // on how often somebody happened to call settle.
    //
    // No second lower clamp is needed on this subtraction: `last_rate_sample_ts`
    // is never written alone — it is only ever set to the same `now` that
    // `last_settle_ts` is — so it is always at most `last_settle_ts`, and
    // `raw > 0` is already proven above.
    let since_sample = now
        .checked_sub(market.last_rate_sample_ts)
        .ok_or(PerpsError::MathOverflow)?;
    if since_sample >= i64::from(market.min_settle_interval_seconds) {
        market.sampled_funding_rate_per_hour = funding_rate_per_hour(
            market.long_oi_usd,
            market.short_oi_usd,
            market.funding_sensitivity,
            market.funding_cap_per_hour,
        )
        .map_err(crate::oracle::map_risk_error)?;
        market.last_rate_sample_ts = now;
    }

    market.last_settle_ts = now;
    Ok(Some(dt))
}

pub fn handle_settle_market(ctx: Context<SettleMarket>) -> Result<()> {
    let clock = Clock::get()?;
    let Some(dt) = accrue(&mut ctx.accounts.market, &ctx.accounts.pool, &clock)? else {
        // Idempotent. A keeper calling twice in a slot, or a cluster clock
        // revised backwards, writes nothing and announces nothing.
        return Ok(());
    };

    let market = &ctx.accounts.market;
    emit!(MarketSettled {
        market: market.key(),
        cum_borrow_index: market.cum_borrow_index,
        cum_funding_index: market.cum_funding_index,
        sampled_funding_rate_per_hour: market.sampled_funding_rate_per_hour,
        last_settle_ts: market.last_settle_ts,
        dt,
    });

    // Deliberately **not** followed by `assert_pool_invariants`. This instruction
    // moves no tokens and changes neither `reserved_quote` nor `quote_deposited`,
    // so it cannot break any of the four — and asserting them anyway would let an
    // unrelated condition stop accrual, which is the same subsidy the absent
    // pause gate exists to prevent.
    Ok(())
}

/// A price after the divergence clamp, with its confidence carried along.
///
/// The two travel together because separating them is a bug. See
/// [`clamp_to_ema_band`].
pub(crate) struct ClampedPrice {
    /// The mid, pulled into the EMA band.
    pub mid: u128,
    /// The confidence interval, rescaled by the same factor the mid moved by.
    pub confidence: u128,
}

/// Pull a spot price into its own EMA band, symmetrically, and rescale the
/// confidence interval with it.
///
/// A **clamp, never a reject**. Rejecting at exit recreates the trap
/// `emergency_close_position` exists to undo, and doing it during an active
/// manipulation is precisely when the trap is most valuable to the manipulator.
/// Symmetric, because an adverse-only clamp stops the pool *paying out* on a
/// manipulated price and does nothing to stop it *charging* on one — and
/// `admin_settle_position` makes a manipulated adverse price a *forced* exit
/// with a fee attached.
///
/// # Why the confidence moves with the mid
///
/// `validate_price` admits a price only when
/// `confidence × 10_000 <= max_confidence_bps × spot`, and `qualify_feed`'s
/// totality check turns that into the guarantee that `execution_price` can
/// always strike a price. Both are statements about the **spot**. Clamping the
/// mid without touching the confidence breaks the link: a spot far above its EMA
/// carries a confidence scaled to the spot, and against the much smaller clamped
/// mid that confidence can exceed it outright — at which point `execution_price`
/// returns `InvalidPrice` and every long in the market is unclosable *and*
/// unliquidatable for as long as the feed keeps publishing that shape.
///
/// Rescaling by the same factor restores the invariant exactly: with
/// `confidence' = floor(confidence × mid / spot)` we get
/// `confidence' × 10_000 <= max_confidence_bps × mid`, so the totality argument
/// holds against the number actually passed rather than against the one it was
/// derived from. It is also the right economics — a proportional band is what a
/// proportional price move implies.
///
/// A zero EMA is not a reference. The price is returned unclamped rather than
/// rejected, for the reason in the first paragraph: turning a broken EMA into a
/// revert at exit would recreate the trap this function exists to avoid. It also
/// stops the band degenerating to `[0, 0]`, which would silently return a mid of
/// zero.
pub(crate) fn clamp_to_ema_band(
    price: &ValidatedPrice,
    ema: u128,
    max_divergence_bps: u16,
) -> Result<ClampedPrice> {
    if ema == 0 || price.price == 0 {
        return Ok(ClampedPrice {
            mid: price.price,
            confidence: price.confidence,
        });
    }

    let divergence = u128::from(max_divergence_bps);
    let denominator = u128::from(crate::BPS_DENOMINATOR);
    let lo = mul_div_floor(
        ema,
        denominator
            .checked_sub(divergence)
            .ok_or(PerpsError::MathOverflow)?,
        denominator,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let hi = mul_div_ceil(
        ema,
        denominator
            .checked_add(divergence)
            .ok_or(PerpsError::MathOverflow)?,
        denominator,
    )
    .map_err(crate::oracle::map_risk_error)?;

    let mid = price.price.clamp(lo, hi);
    if mid == price.price {
        return Ok(ClampedPrice {
            mid,
            confidence: price.confidence,
        });
    }

    // Floor, so the rescaled band is never wider than the proportional one —
    // the direction the inequality above needs.
    let confidence =
        mul_div_floor(price.confidence, mid, price.price).map_err(crate::oracle::map_risk_error)?;

    Ok(ClampedPrice { mid, confidence })
}

pub fn handle_refresh_market_price(ctx: Context<RefreshMarketPrice>) -> Result<()> {
    let clock = Clock::get()?;
    let (price, ema) = crate::oracle::load_price_and_ema(
        &ctx.accounts.price_update,
        &ctx.accounts.market.feed_id,
        &ctx.accounts.market.trading_guards(),
        &clock,
    )?;
    let clamped = clamp_to_ema_band(&price, ema, ctx.accounts.market.max_divergence_bps)?;

    let market = &mut ctx.accounts.market;
    market.last_good_price = clamped.mid;
    market.last_good_price_ts = clock.unix_timestamp;

    emit!(MarketPriceRefreshed {
        market: market.key(),
        last_good_price: market.last_good_price,
        last_good_price_ts: market.last_good_price_ts,
    });

    // No invariant assertion: this instruction touches no value, moves no
    // tokens, and grants its caller nothing.
    Ok(())
}

/// Accounts for [`crate::sakura_perps::qualify_feed`].
#[derive(Accounts)]
#[instruction(params: QualifyFeedParams)]
pub struct QualifyFeed<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + QualifiedFeed::INIT_SPACE,
        seeds = [b"feed", params.feed_id.as_ref()],
        bump,
    )]
    pub feed: Box<Account<'info, QualifiedFeed>>,

    /// The exact account this feed is qualified against. Recorded, not trusted.
    /// Unconstrained here because this instruction is what *establishes* the
    /// binding — every later instruction pins against `market.price_update`.
    pub price_update: Box<Account<'info, PriceUpdateV2>>,

    pub system_program: Program<'info, System>,
}

/// Accounts for [`crate::sakura_perps::set_feed_revoked`].
#[derive(Accounts)]
pub struct SetFeedRevoked<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(mut, seeds = [b"feed", feed.feed_id.as_ref()], bump = feed.bump)]
    pub feed: Box<Account<'info, QualifiedFeed>>,
}

/// Accounts for [`crate::sakura_perps::create_market`].
#[derive(Accounts)]
pub struct CreateMarket<'info> {
    /// `mut`: `num_markets` is incremented.
    #[account(mut, seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    /// Rent only, and no authority. Creating a market grants nothing, because
    /// the market is born quarantined.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        seeds = [b"feed", feed.feed_id.as_ref()],
        bump = feed.bump,
        constraint = !feed.revoked @ PerpsError::FeedRevoked,
    )]
    pub feed: Box<Account<'info, QualifiedFeed>>,

    #[account(
        init,
        payer = payer,
        space = 8 + Market::INIT_SPACE,
        seeds = [b"market", feed.feed_id.as_ref()],
        bump,
    )]
    pub market: Box<Account<'info, Market>>,

    pub system_program: Program<'info, System>,
}

/// Accounts for [`crate::sakura_perps::set_risk_params`].
#[derive(Accounts)]
pub struct SetRiskParams<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
}

/// Accounts for [`crate::sakura_perps::settle_market`].
#[derive(Accounts)]
pub struct SettleMarket<'info> {
    /// Read-only, and required: borrow accrual is a function of pool-wide
    /// utilisation.
    #[account(seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
}

/// Accounts for [`crate::sakura_perps::refresh_market_price`].
///
/// No signer, no exchange, no pool. There is nothing here to authorise.
#[derive(Accounts)]
pub struct RefreshMarketPrice<'info> {
    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,

    #[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
}

#[event]
pub struct FeedQualified {
    pub feed: Pubkey,
    pub feed_id: [u8; 32],
    pub price_update: Pubkey,
    pub expected_exponent: i32,
    pub asset_decimals: u8,
}

#[event]
pub struct FeedRevocationChanged {
    pub feed: Pubkey,
    pub feed_id: [u8; 32],
    pub revoked: bool,
}

#[event]
pub struct MarketCreated {
    pub market: Pubkey,
    pub feed_id: [u8; 32],
    pub market_index: u32,
    pub price_update: Pubkey,
}

/// The full risk-parameter block, plus the quarantine transition.
///
/// Every field is carried rather than a diff: an indexer that had to reconstruct
/// the current parameters by replaying diffs would get it wrong the first time a
/// transaction it missed landed.
#[event]
pub struct RiskParamsSet {
    pub market: Pubkey,
    pub initial_margin_bps: u16,
    pub maintenance_margin_bps: u16,
    pub liquidation_fee_bps: u16,
    pub max_profit_bps: u16,
    pub spread_bps: u16,
    pub open_fee_bps: u16,
    pub close_fee_bps: u16,
    pub max_oi_usd: u128,
    pub max_oracle_drift_bps: u16,
    pub min_position_size_base: u64,
    pub min_notional_usd: u128,
    pub min_collateral_usd: u128,
    pub borrow_rate_per_hour: u128,
    pub funding_sensitivity: u128,
    pub funding_cap_per_hour: u128,
    pub max_settle_window_seconds: u32,
    pub min_settle_interval_seconds: u32,
    pub quarantined: bool,
    pub quarantined_ts: i64,
}

#[event]
pub struct MarketSettled {
    pub market: Pubkey,
    pub cum_borrow_index: u128,
    pub cum_funding_index: i128,
    pub sampled_funding_rate_per_hour: i128,
    pub last_settle_ts: i64,
    /// The **clamped** Δt actually accrued, not the wall-clock gap.
    pub dt: u64,
}

#[event]
pub struct MarketPriceRefreshed {
    pub market: Pubkey,
    pub last_good_price: u128,
    pub last_good_price_ts: i64,
}

/// Host-side tests, and the fixtures the position and pool tests share.
///
/// They run under `cargo test -p sakura-perps --lib`: no LiteSVM, no compiled
/// `.so`, no Solana toolchain. The fixtures are `pub(crate)` and used from
/// `position::tests` and `pool::tests` so there is one canonical activated
/// market rather than three copies that can drift apart.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sakura_perps_risk::scale::{PRICE_SCALE, USD_SCALE};

    /// A pool with the given tracked equity and reserve, and nothing else.
    pub(crate) fn test_pool(quote_deposited: u64, reserved_quote: u64) -> Pool {
        Pool {
            bump: 255,
            vault_bump: 254,
            share_mint: Pubkey::default(),
            quote_vault: Pubkey::default(),
            total_shares: quote_deposited,
            quote_deposited,
            locked_quote: 0,
            pending_protocol_fees: 0,
            reserved_quote,
            deposit_fee_bps: 0,
            withdraw_fee_bps: 0,
            withdraw_delay_seconds: 60,
            max_utilization_bps: crate::M5_MAX_UTILIZATION_BPS,
            max_aum_quote: u64::MAX,
            _reserved: [0u8; 128],
        }
    }

    /// An activated market whose parameters pass every one of
    /// `set_risk_params`'s ten validations.
    ///
    /// Chosen to be legal rather than round: a fixture that could not survive
    /// the instruction that produces it would let the ledger tests pass against
    /// a market no admin could ever create.
    pub(crate) fn test_market() -> Market {
        Market {
            bump: 255,
            market_index: 0,
            feed_id: [7u8; 32],
            price_update: Pubkey::default(),
            expected_exponent: -8,
            asset_decimals: 9,
            min_price: PRICE_SCALE,
            max_price: 1_000_000 * PRICE_SCALE,
            trading_max_age_seconds: 30,
            trading_max_age_slots: 100,
            trading_max_future_skew_seconds: 5,
            trading_max_confidence_bps: 100,
            liquidation_max_age_seconds: 120,
            liquidation_max_age_slots: 400,
            liquidation_max_future_skew_seconds: 10,
            liquidation_max_confidence_bps: 500,
            max_divergence_bps: 500,

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

            borrow_rate_per_hour: 100_000,
            funding_sensitivity: 1_000_000,
            funding_cap_per_hour: 1_000_000,
            max_settle_window_seconds: 3_600,
            min_settle_interval_seconds: 60,
            cum_borrow_index: 0,
            borrow_remainder_carry: 0,
            cum_funding_index: 0,
            sampled_funding_rate_per_hour: 0,
            last_settle_ts: 1_700_000_000,
            last_rate_sample_ts: 1_700_000_000,

            long_oi_usd: 0,
            short_oi_usd: 0,
            long_positions: 0,
            short_positions: 0,
            locked_quote: 0,
            reserved_quote: 0,
            cum_bad_debt_usd: 0,
            quarantined_ts: 0,
            last_good_price: 0,
            last_good_price_ts: 0,
            _reserved: [0u8; 96],
        }
    }

    fn clock_at(unix_timestamp: i64) -> Clock {
        Clock {
            slot: 0,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp,
        }
    }

    fn priced(price: u128, confidence: u128) -> ValidatedPrice {
        ValidatedPrice {
            price,
            confidence,
            publish_time: 0,
            posted_slot: 0,
        }
    }

    /// A backwards clock accrues nothing and **writes nothing**.
    ///
    /// `Clock::unix_timestamp` is a stake-weighted vote estimate and is not
    /// monotonic, so this is a state the cluster genuinely reaches. The failure
    /// it guards against is not a small mis-accrual: an unguarded `as u64` on a
    /// negative interval is about 1.8e19 seconds, which after the settle-window
    /// clamp still moves both indices and — worse — would write `last_settle_ts`
    /// backwards, so the next honest call would re-charge the whole gap.
    #[test]
    fn a_backwards_clock_accrues_nothing_and_writes_nothing() {
        let pool = test_pool(1_000_000_000, 100_000_000);
        let mut market = test_market();
        // Something to lose: a rate, a carry, and indices already advanced.
        market.cum_borrow_index = 12_345;
        market.borrow_remainder_carry = 999;
        market.cum_funding_index = -4_242;
        market.sampled_funding_rate_per_hour = 500_000;
        let before = (
            market.cum_borrow_index,
            market.borrow_remainder_carry,
            market.cum_funding_index,
            market.last_settle_ts,
            market.last_rate_sample_ts,
            market.sampled_funding_rate_per_hour,
        );

        for revision in [-1i64, -60, -86_400, -1_700_000_000] {
            let clock = clock_at(market.last_settle_ts + revision);
            assert_eq!(
                accrue(&mut market, &pool, &clock).unwrap(),
                None,
                "a backwards clock must accrue nothing"
            );
            assert_eq!(
                (
                    market.cum_borrow_index,
                    market.borrow_remainder_carry,
                    market.cum_funding_index,
                    market.last_settle_ts,
                    market.last_rate_sample_ts,
                    market.sampled_funding_rate_per_hour,
                ),
                before,
                "a backwards clock must write nothing"
            );
        }

        // The same instant is the keeper-calls-twice case, and it is also a
        // no-op rather than a zero-length accrual that still writes.
        let clock = clock_at(market.last_settle_ts);
        assert_eq!(accrue(&mut market, &pool, &clock).unwrap(), None);
    }

    /// Δt is clamped to the settle window, so an unattended market cannot charge
    /// a month in one jump against a utilisation figure from its far end.
    #[test]
    fn a_long_gap_accrues_only_the_settle_window() {
        let pool = test_pool(1_000_000_000, 200_000_000);
        let mut market = test_market();
        let start = market.last_settle_ts;
        let clock = clock_at(start + 30 * 24 * 60 * 60);

        let dt = accrue(&mut market, &pool, &clock).unwrap().unwrap();
        assert_eq!(dt, u64::from(market.max_settle_window_seconds));
        // The timestamp still advances to *now*: the clamp bounds what is
        // charged, not what has elapsed. Otherwise the unaccrued remainder would
        // be charged by the next call, defeating the clamp entirely.
        assert_eq!(market.last_settle_ts, clock.unix_timestamp);
    }

    /// Settling every second accrues what settling once an hour does.
    ///
    /// The remainder carry is what makes that true, and dropping it is the
    /// regression the risk crate's own test names: 3 600 one-second calls each
    /// floored to zero while the single call accrued 3 590.
    #[test]
    fn settle_cadence_does_not_change_what_borrow_accrues() {
        let pool = test_pool(1_000_000_000, 35_900_000);

        let mut once = test_market();
        let start = once.last_settle_ts;
        accrue(&mut once, &pool, &clock_at(start + 3_600)).unwrap();

        let mut often = test_market();
        for second in 1..=3_600i64 {
            accrue(&mut often, &pool, &clock_at(start + second)).unwrap();
        }

        assert_eq!(once.cum_borrow_index, often.cum_borrow_index);
        assert!(
            once.cum_borrow_index > 0,
            "non-vacuous: something must have accrued"
        );
    }

    /// The clamp is symmetric, and it never rejects.
    #[test]
    fn the_ema_clamp_pulls_from_both_sides_and_never_rejects() {
        let ema = 100 * PRICE_SCALE;

        // Above the band, pulled down to the ceiling.
        let high = clamp_to_ema_band(&priced(200 * PRICE_SCALE, 0), ema, 500).unwrap();
        assert_eq!(high.mid, 105 * PRICE_SCALE);

        // Below the band, pulled up to the floor. An adverse-only clamp would
        // leave this one alone and close half of the protection.
        let low = clamp_to_ema_band(&priced(50 * PRICE_SCALE, 0), ema, 500).unwrap();
        assert_eq!(low.mid, 95 * PRICE_SCALE);

        // Inside the band, untouched.
        let inside = clamp_to_ema_band(&priced(102 * PRICE_SCALE, 7), ema, 500).unwrap();
        assert_eq!(inside.mid, 102 * PRICE_SCALE);
        assert_eq!(inside.confidence, 7);

        // A broken EMA is not a reference, and is not a rejection either.
        let no_reference = clamp_to_ema_band(&priced(200 * PRICE_SCALE, 3), 0, 500).unwrap();
        assert_eq!(no_reference.mid, 200 * PRICE_SCALE);
        assert_eq!(no_reference.confidence, 3);
    }

    /// The clamp rescales confidence, so `execution_price` stays total.
    ///
    /// This is the composition the two oracle-priced exits actually perform, and
    /// the case that used to break it: a spot far above its EMA, admissible
    /// under the liquidation confidence gate, whose spot-scaled confidence
    /// exceeds the clamped mid outright. Unrescaled, `execution_price` returns
    /// `InvalidPrice` for a long close and the position is neither closable nor
    /// liquidatable.
    #[test]
    fn a_clamped_price_is_still_strikeable_at_every_legal_spread() {
        use sakura_perps_risk::position::{execution_price, PriceDirection, Side};

        let ema = 100 * PRICE_SCALE;
        // Admissible at a 500 bps gate: 110 × 10_000 <= 500 × 2_200.
        let spot = priced(2_200 * PRICE_SCALE, 110 * PRICE_SCALE);
        let clamped = clamp_to_ema_band(&spot, ema, 500).unwrap();
        assert_eq!(clamped.mid, 105 * PRICE_SCALE, "the clamp really bit");
        assert!(
            clamped.confidence < spot.confidence,
            "the confidence came down with the mid"
        );
        // Unrescaled, this is the failure: the old confidence alone exceeds the
        // clamped mid, so the adverse adjustment can never be below it.
        assert!(spot.confidence > clamped.mid);

        for side in [Side::Long, Side::Short] {
            for spread_bps in [0u16, 1, 10, 250, MAX_SPREAD_BPS] {
                execution_price(
                    side,
                    PriceDirection::Close,
                    clamped.mid,
                    clamped.confidence,
                    spread_bps,
                )
                .unwrap_or_else(|err| {
                    panic!("unstrikeable at {spread_bps}bps after clamping: {err:?}")
                });
            }
        }
    }

    /// The rescaled confidence still satisfies the gate it was admitted under.
    ///
    /// That inequality is what `qualify_feed` validation 6 turns into totality,
    /// so it is asserted directly rather than only through its consequence.
    #[test]
    fn the_rescaled_confidence_still_satisfies_the_confidence_gate() {
        let denominator = u128::from(crate::BPS_DENOMINATOR);
        for gate_bps in [1u16, 100, 500, 2_000, 9_499] {
            for spot_price in [PRICE_SCALE, 137 * PRICE_SCALE, 2_200 * PRICE_SCALE] {
                // The widest confidence the gate admits at this price.
                let confidence = spot_price * u128::from(gate_bps) / denominator;
                for ema in [PRICE_SCALE, 100 * PRICE_SCALE, 100_000 * PRICE_SCALE] {
                    for divergence_bps in [1u16, 500, 5_000, 9_999] {
                        let clamped =
                            clamp_to_ema_band(&priced(spot_price, confidence), ema, divergence_bps)
                                .unwrap();
                        assert!(
                            clamped.confidence * denominator <= u128::from(gate_bps) * clamped.mid,
                            "gate {gate_bps}, spot {spot_price}, ema {ema}, \
                             divergence {divergence_bps}"
                        );
                    }
                }
            }
        }
    }
}
