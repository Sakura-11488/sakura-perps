//! Funding and borrow accrual.
//!
//! Both are index-based: the market carries a cumulative value, a position
//! records the index at the moment it opened, and what is owed is the
//! difference multiplied by notional. This makes settlement O(1) per position
//! and — more importantly — makes it impossible to owe a different amount
//! depending on how often somebody happened to call the settle instruction.
//!
//! # Why accrual is integrated over elapsed time
//!
//! If funding were a discrete payment applied at a settlement instant, a trader
//! could open a large one-sided position immediately before settlement, collect,
//! and close immediately after. Accruing over `Δt` removes the instant to aim
//! at. Three further defences belong in the program rather than here: settle at
//! the head of every open, close and liquidate so `Δt` is always small; enforce
//! a minimum interval so a settler who has just moved open interest cannot pick
//! the sampling moment; and cap `Δt` per call so an outage cannot accrue an
//! unbounded jump.
//!
//! There is also a design constraint worth writing down, because it is easy to
//! violate by tuning parameters innocently: **round-trip fees must exceed the
//! maximum funding accruable in the minimum holding period.** At 6 bps open plus
//! 6 bps close against a 10 bps-per-hour funding cap, farming funding for an
//! hour is structurally unprofitable. Lower the fees or raise the cap without
//! checking this and you have created a money pump.

use crate::error::RiskError;
use crate::math::{mul_div_ceil, mul_div_floor, mul_div_i128};
use crate::scale::{RATE_SCALE, RATE_SCALE_I, SECONDS_PER_HOUR};

/// Change in the cumulative borrow index over an elapsed period.
///
/// Borrow is charged on notional for the use of pool liquidity, scales with
/// utilisation, and is always paid by the trader to the pool. It is never
/// negative, so it needs no sign handling — which is why Jupiter runs on borrow
/// fees alone with no funding rate at all, and why it is worth shipping first.
///
/// `rate_per_hour` and the returned delta are at [`RATE_SCALE`].
pub fn borrow_index_delta(
    rate_per_hour: u128,
    utilization_bps: u128,
    elapsed_seconds: u64,
) -> Result<u128, RiskError> {
    if elapsed_seconds == 0 || rate_per_hour == 0 || utilization_bps == 0 {
        return Ok(0);
    }
    // rate × utilisation × Δt / (10_000 × 3600), computed as two mul_divs so no
    // single product has to fit alone.
    let scaled_by_utilization = mul_div_floor(rate_per_hour, utilization_bps, 10_000)?;
    mul_div_floor(
        scaled_by_utilization,
        elapsed_seconds as u128,
        SECONDS_PER_HOUR,
    )
}

/// Borrow owed by a position, rounded **up**.
///
/// `entry_index` is the cumulative index recorded when the position last
/// settled. An index that has gone backwards is a corrupted market, not a
/// refund, so it is rejected rather than producing a credit.
pub fn borrow_owed(
    notional_usd: u128,
    current_index: u128,
    entry_index: u128,
) -> Result<u128, RiskError> {
    if current_index < entry_index {
        return Err(RiskError::MathOverflow);
    }
    let delta = current_index - entry_index;
    if delta == 0 {
        return Ok(0);
    }
    mul_div_ceil(notional_usd, delta, RATE_SCALE)
}

/// Instantaneous funding rate per hour from open-interest skew, clamped.
///
/// Positive means longs pay shorts. The clamp is not decoration: an unclamped
/// rate derived from a skew ratio can spike arbitrarily when one side is nearly
/// empty, and that spike is directly extractable.
///
/// Returns `0` when there is no open interest at all, which is the only sensible
/// reading of an empty market.
pub fn funding_rate_per_hour(
    long_oi_usd: u128,
    short_oi_usd: u128,
    sensitivity: u128,
    cap_per_hour: u128,
) -> Result<i128, RiskError> {
    let total = long_oi_usd
        .checked_add(short_oi_usd)
        .ok_or(RiskError::MathOverflow)?;
    if total == 0 {
        return Ok(0);
    }

    let (skew, longs_pay) = if long_oi_usd >= short_oi_usd {
        (long_oi_usd - short_oi_usd, true)
    } else {
        (short_oi_usd - long_oi_usd, false)
    };

    let magnitude = mul_div_floor(sensitivity, skew, total)?.min(cap_per_hour);
    let magnitude = i128::try_from(magnitude).map_err(|_| RiskError::MathOverflow)?;

    Ok(if longs_pay { magnitude } else { -magnitude })
}

/// Change in the cumulative funding index over an elapsed period.
///
/// Signed, at [`RATE_SCALE`]. Truncates toward zero, so neither side gains from
/// the rounding by choosing which way the rate points.
pub fn funding_index_delta(rate_per_hour: i128, elapsed_seconds: u64) -> Result<i128, RiskError> {
    if elapsed_seconds == 0 || rate_per_hour == 0 {
        return Ok(0);
    }
    mul_div_i128(
        rate_per_hour,
        elapsed_seconds as i128,
        SECONDS_PER_HOUR as i128,
    )
}

/// Funding owed by a position. Positive means the trader pays.
///
/// **Accrued against entry notional, not current notional.** If it were charged
/// on the notional at settlement time, the amount owed would move with the price
/// and a trader could choose when to settle in order to shrink it. Entry
/// notional is fixed at the moment the position last settled and cannot be
/// gamed.
pub fn funding_owed(
    entry_notional_usd: u128,
    current_index: i128,
    entry_index: i128,
) -> Result<i128, RiskError> {
    let delta = current_index
        .checked_sub(entry_index)
        .ok_or(RiskError::MathOverflow)?;
    if delta == 0 {
        return Ok(0);
    }
    let notional = i128::try_from(entry_notional_usd).map_err(|_| RiskError::MathOverflow)?;
    mul_div_i128(notional, delta, RATE_SCALE_I)
}

/// Whether the fee schedule makes funding-farming unprofitable over a period.
///
/// Encodes the constraint from the module docs so it can be asserted in tests
/// and at market configuration rather than being left as a comment nobody
/// re-checks when the parameters are tuned.
pub fn fees_dominate_funding(
    open_fee_bps: u16,
    close_fee_bps: u16,
    funding_cap_per_hour: u128,
    holding_period_seconds: u64,
) -> Result<bool, RiskError> {
    let round_trip_bps = (open_fee_bps as u128)
        .checked_add(close_fee_bps as u128)
        .ok_or(RiskError::MathOverflow)?;

    // Maximum funding accruable over the period, expressed in bps of notional.
    let max_funding = mul_div_ceil(
        funding_cap_per_hour,
        holding_period_seconds as u128,
        SECONDS_PER_HOUR,
    )?;
    let max_funding_bps = mul_div_ceil(max_funding, 10_000, RATE_SCALE)?;

    Ok(round_trip_bps > max_funding_bps)
}
