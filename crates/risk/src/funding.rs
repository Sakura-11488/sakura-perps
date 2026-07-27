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

/// One step of borrow-index accrual, with the undivided remainder to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BorrowAccrual {
    /// Amount to add to the market's cumulative borrow index, at [`RATE_SCALE`].
    pub index_delta: u128,
    /// Undivided leftover, to be passed back in as `carried_remainder` on the
    /// next call. The market must store this; dropping it reintroduces the leak
    /// described below.
    pub remainder: u128,
}

/// Change in the cumulative borrow index over an elapsed period.
///
/// Borrow is charged on notional for the use of pool liquidity, scales with
/// utilisation, and is always paid by the trader to the pool. It is never
/// negative, so it needs no sign handling — which is why Jupiter runs on borrow
/// fees alone with no funding rate at all, and why it is worth shipping first.
///
/// # Why this carries a remainder
///
/// The obvious implementation divides by `10_000 × 3600` and floors. That is
/// **sub-additive**, and the loss is not marginal: with `rate_per_hour` 100 000
/// and utilisation 359 bps, one call covering 3 600 seconds yields an index
/// delta of 3 590, while 3 600 calls covering one second each yield **exactly
/// zero** — every individual call floors to nothing, so the index never moves.
///
/// That leak is worst under precisely the operating mode this module recommends
/// three paragraphs above: settling at the head of every open, close and
/// liquidate, so `Δt` is always small. No attacker is required; a busy market
/// destroys its own primary revenue.
///
/// Rounding up instead would be worse, not better. It would over-accrue by up
/// to a full index unit *per call*, which at a high settle cadence leaks in the
/// opposite direction — against traders rather than against the pool. Neither
/// direction is acceptable for a cumulative index.
///
/// Carrying the remainder is the only correct answer: accrual becomes exactly
/// additive, so the total charged over any interval is identical however many
/// times it was settled. `borrow_owed` still rounds up when converting an index
/// delta into an amount owed, which is where the pool's favour belongs.
///
/// `rate_per_hour` and the returned delta are at [`RATE_SCALE`].
pub fn borrow_index_delta(
    rate_per_hour: u128,
    utilization_bps: u128,
    elapsed_seconds: u64,
    carried_remainder: u128,
) -> Result<BorrowAccrual, RiskError> {
    // `10_000` for basis points, `3_600` for seconds-to-hours. Divided once, at
    // the end, so nothing is truncated mid-computation.
    let denominator = 10_000u128
        .checked_mul(SECONDS_PER_HOUR)
        .ok_or(RiskError::MathOverflow)?;

    if elapsed_seconds == 0 || rate_per_hour == 0 || utilization_bps == 0 {
        // No accrual, but the carry must survive: zeroing it here would discard
        // value every time utilisation briefly touched zero.
        return Ok(BorrowAccrual {
            index_delta: 0,
            remainder: carried_remainder,
        });
    }

    let numerator = rate_per_hour
        .checked_mul(utilization_bps)
        .and_then(|value| value.checked_mul(elapsed_seconds as u128))
        .and_then(|value| value.checked_add(carried_remainder))
        .ok_or(RiskError::MathOverflow)?;

    Ok(BorrowAccrual {
        index_delta: numerator / denominator,
        remainder: numerator % denominator,
    })
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
///
/// This looks like the same shape as [`borrow_index_delta`] and it deliberately
/// does **not** carry a remainder. The difference is who the money moves
/// between. Borrow flows from trader to pool, so under-accruing is a loss to the
/// pool that nobody balances. Funding flows from one side of the market to the
/// other: both sides read the same index, so truncation shrinks the transfer
/// symmetrically and nobody profits. Sub-additivity here weakens funding
/// slightly as a balancing force at very high settle cadence — an economic
/// tuning question for the rate parameters, not a leak.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the borrow-accrual leak found by adversarial review.
    ///
    /// With `rate_per_hour = 100_000` and utilisation 359 bps, the original
    /// implementation returned an index delta of 3_590 for a single 3_600-second
    /// call and **exactly zero** for 3_600 one-second calls: each individual
    /// call floored `3_590 × 1 / 3_600` to nothing, so the index never advanced
    /// and the pool collected no borrow fees at all.
    ///
    /// Priced through a $1,000,000 notional that is 3.59 dollars an hour of
    /// revenue becoming zero, purely as a function of how often the market was
    /// settled — and worst under the settle-often cadence this module
    /// recommends elsewhere.
    #[test]
    fn subdividing_an_interval_does_not_change_the_accrual() {
        const RATE: u128 = 100_000;
        const UTILIZATION_BPS: u128 = 359;

        let single = borrow_index_delta(RATE, UTILIZATION_BPS, 3_600, 0).unwrap();
        assert_eq!(single.index_delta, 3_590);

        let mut total = 0u128;
        let mut carry = 0u128;
        for _ in 0..3_600 {
            let step = borrow_index_delta(RATE, UTILIZATION_BPS, 1, carry).unwrap();
            total += step.index_delta;
            carry = step.remainder;
        }

        assert_eq!(
            total, 3_590,
            "3600 one-second steps must accrue exactly what one 3600-second step does"
        );
    }

    /// The other end of the same defect: a rate low enough that a per-second
    /// step used to floor to zero forever.
    #[test]
    fn a_rate_too_small_to_accrue_per_second_still_accrues_over_time() {
        // 20_000 × 1_500 / 10_000 = 3_000 per hour, which is under 3_600 and so
        // floored to zero on every one-second call before the remainder carry.
        let mut total = 0u128;
        let mut carry = 0u128;
        for _ in 0..3_600 {
            let step = borrow_index_delta(20_000, 1_500, 1, carry).unwrap();
            total += step.index_delta;
            carry = step.remainder;
        }
        assert_eq!(total, 3_000);
    }
}
