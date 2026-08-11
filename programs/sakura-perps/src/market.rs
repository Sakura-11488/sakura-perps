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

use anchor_lang::prelude::*;
use sakura_perps_risk::oracle::OracleGuards;

/// Upper bound on a market's spread, in basis points.
///
/// A spread is charged against the trader on both legs; leaving it unbounded
/// would let an admin make a market that cannot be traded profitably at any
/// price while still appearing open.
pub const MAX_SPREAD_BPS: u16 = 500;

/// Upper bound on either trade fee, in basis points.
pub const MAX_TRADE_FEE_BPS: u16 = 500;

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
    pub _reserved: [u8; 128],
}

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
