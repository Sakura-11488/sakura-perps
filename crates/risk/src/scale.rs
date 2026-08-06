//! Fixed-point scales and the rounding policy.
//!
//! Everything in this crate is fixed-point integer arithmetic. There are no
//! floats anywhere, and there never will be: floating point is non-associative,
//! its rounding is not controllable, and its presence in margin maths is an
//! audit finding on its own.
//!
//! # Scales
//!
//! | Quantity | Type | Scale | Why |
//! |---|---|---|---|
//! | Token amounts | `u64` | mint base units | matches SPL, bounded by supply |
//! | USD values | `u128` | `1e6` (micro-dollars) | `size × price` overflows `u64` by 13 orders of magnitude |
//! | Prices | `u128` | `1e10` | Pyth crypto feeds publish exponent `-8`; `1e10` keeps seven significant figures even for a $0.00028 asset while `price × size` stays well inside `u128` |
//! | Rates and ratios | `u128` | `1e9` | borrow index, utilisation |
//! | Cumulative funding | `i128` | `1e9` per unit USD notional | signed: longs pay shorts or the reverse |
//! | Basis points | `u16` | `1e-4` | max 10 000 |
//!
//! Mixing scales silently is the single easiest way to be wrong by a factor of
//! ten thousand, so every conversion goes through a named function rather than a
//! bare multiplication.
//!
//! # Rounding policy
//!
//! **Every rounding decision resolves in favour of the pool, never the
//! individual user.** Stated once here and enforced at every call site by
//! choosing `floor` or `ceil` deliberately rather than letting integer division
//! decide by accident.
//!
//! | Quantity | Direction | Reason |
//! |---|---|---|
//! | Fees charged to a user | up | the pool must never under-collect |
//! | Collateral credited to a user | down | never credit more than was received |
//! | Shares minted on deposit | down | never mint more claim than value deposited |
//! | Assets returned on withdraw | down | never pay out more than the share is worth |
//! | Trader profit (pool pays) | down | |
//! | Trader loss (pool receives) | up in magnitude | |
//! | Margin requirement | up | |
//!
//! A position sitting exactly on the liquidation threshold **is** liquidatable
//! (`<=`, not `<`). Ties go to the pool.

/// Scale for USD-denominated values: micro-dollars.
pub const USD_SCALE: u128 = 1_000_000;

/// Scale for prices.
///
/// Chosen so a $0.00028 asset still carries seven significant figures while
/// `price × size` stays far inside `u128`.
pub const PRICE_SCALE: u128 = 10_000_000_000;

/// Scale for rates, ratios and cumulative indices.
pub const RATE_SCALE: u128 = 1_000_000_000;

/// Signed form of [`RATE_SCALE`], for cumulative funding.
pub const RATE_SCALE_I: i128 = 1_000_000_000;

/// Basis-point denominator. 10 000 bps == 100%.
pub const BPS_DENOMINATOR: u128 = 10_000;

/// Seconds in an hour, for rate accrual.
pub const SECONDS_PER_HOUR: u128 = 3_600;

/// Largest exponent [`crate::math::pow10`] will produce.
///
/// `10^38` is the largest power of ten below `u128::MAX` (~3.4 × 10^38).
pub const MAX_POW10: u32 = 38;

// ── Collateral base units ↔ micro-dollars ───────────────────────────────────
//
// The program has so far treated token base units and micro-dollars as the same
// number. That holds for a 6-decimal mint and for no other, and
// `Exchange.collateral_decimals` is read at runtime precisely because the mint
// is not guaranteed to be USDC. Every crossing goes through one of these three,
// with the rounding direction chosen deliberately at the call site rather than
// inherited from whichever way integer division happened to fall.

use crate::error::RiskError;
use crate::math::{mul_div_ceil, mul_div_floor, pow10};

/// Collateral base units → micro-dollars, rounded **down**.
///
/// Down because this values what a user holds: never credit more than the
/// tokens are worth.
pub fn quote_to_usd_floor(amount_quote: u128, collateral_decimals: u8) -> Result<u128, RiskError> {
    let unit = pow10(collateral_decimals as u32)?;
    mul_div_floor(amount_quote, USD_SCALE, unit)
}

/// Micro-dollars → collateral base units, rounded **down**.
///
/// Down because this is what the pool pays out — trader profit, collateral
/// returned. Never pay more than the dollar figure earned.
pub fn usd_to_quote_floor(amount_usd: u128, collateral_decimals: u8) -> Result<u128, RiskError> {
    let unit = pow10(collateral_decimals as u32)?;
    mul_div_floor(amount_usd, unit, USD_SCALE)
}

/// Micro-dollars → collateral base units, rounded **up**.
///
/// Up because this is what the pool collects — fees, margin required. Never
/// under-collect.
pub fn usd_to_quote_ceil(amount_usd: u128, collateral_decimals: u8) -> Result<u128, RiskError> {
    let unit = pow10(collateral_decimals as u32)?;
    mul_div_ceil(amount_usd, unit, USD_SCALE)
}
