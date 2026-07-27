//! Oracle price validation.
//!
//! This module holds the *policy* — what makes a price safe to trade on — with
//! no knowledge of Pyth account layouts or Solana at all. The program
//! deserialises the feed and hands the raw numbers here. That split means every
//! rejection path can be tested exhaustively in milliseconds, including the ones
//! that are awkward to reach on chain: a price published in the future, an
//! exponent that changed underneath you, a confidence interval that quietly
//! widened during a market dislocation.
//!
//! # Why this is the most safety-critical code in the protocol
//!
//! In an oracle-and-pool venue the oracle *is* the counterparty's opinion of
//! value. Nearly every large perpetuals exploit reduces to trading against a
//! price the protocol should not have believed. There is no clever margin
//! formula that survives a wrong mark price, so the checks here are deliberately
//! paranoid and deliberately fail closed.
//!
//! # What is checked, and why each one exists
//!
//! | Check | Failure it prevents |
//! |---|---|
//! | positive mantissa | a negative or zero price rendering all margin maths meaningless |
//! | exponent matches the qualified value | a feed silently rescaling every price by orders of magnitude |
//! | publish time not in the future | a far-future timestamp making a stale price look fresh forever |
//! | publish time within `max_age_seconds` | trading on a price the upstream stopped updating |
//! | posted slot within `max_age_slots` | a price that is fresh upstream but landed on this chain long ago |
//! | confidence within `max_confidence_bps` | trading through a dislocation the oracle itself is unsure about |
//! | price inside the sanity band | a repointed feed, or a catastrophic upstream error |
//!
//! The future-timestamp check deserves particular note because it is the one
//! most often missed. A naive staleness test computes `now - publish_time` and
//! rejects when that exceeds a limit. If `publish_time` is far in the future the
//! difference is negative, the comparison passes, and the price is accepted
//! forever — the check inverts into a permanent bypass.
//!
//! # Two clocks, not one
//!
//! `publish_time` describes when the *upstream* believed the price. `posted_slot`
//! describes when it landed on *this* chain. They fail independently: a feed can
//! stop being updated while its last message still looks recent, and a message
//! can carry a fresh timestamp while sitting unposted for many slots. Checking
//! only one leaves the other open, so both are checked.

use crate::error::RiskError;
use crate::math::pow10;
use crate::scale::BPS_DENOMINATOR;

/// Decimal exponent of [`crate::scale::PRICE_SCALE`]. `1e10` is `10^10`.
const PRICE_SCALE_EXPONENT: i32 = 10;

/// Widest exponent range a feed may declare.
///
/// Pyth crypto feeds publish `-8`; equities use `-5`. Anything outside this band
/// is either a misconfiguration or a feed that is not what it claims to be, and
/// bounding it keeps [`pow10`] far from its own limits.
pub const MIN_EXPONENT: i32 = -18;
/// See [`MIN_EXPONENT`].
pub const MAX_EXPONENT: i32 = 0;

/// A price exactly as the oracle reports it, before any validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPrice {
    /// Signed mantissa. Pyth's `price` field, which genuinely can be negative
    /// for some feeds, so it is not assumed positive.
    pub mantissa: i64,
    /// Confidence interval, in the same units and exponent as `mantissa`.
    pub confidence: u64,
    /// Decimal exponent: the true value is `mantissa × 10^exponent`.
    pub exponent: i32,
    /// Unix seconds at which the upstream published this price.
    pub publish_time: i64,
    /// Slot at which this price landed on chain.
    pub posted_slot: u64,
}

/// Per-market limits on what counts as an acceptable price.
///
/// These are stored per market rather than as constants precisely so that
/// liquidation can run under looser limits than opening a position. Refusing to
/// liquidate because a price is a few seconds staler than an opening threshold
/// would allow is how bad debt accumulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleGuards {
    /// Maximum age by upstream publish time.
    pub max_age_seconds: u32,
    /// Maximum age by slots since the price landed on chain.
    pub max_age_slots: u64,
    /// Tolerance for a publish time ahead of the local clock. Small, to absorb
    /// ordinary skew without opening the future-timestamp bypass.
    pub max_future_skew_seconds: u32,
    /// Maximum confidence interval as a fraction of price, in basis points.
    pub max_confidence_bps: u16,
    /// Lower bound of the sanity band, at [`crate::scale::PRICE_SCALE`].
    pub min_price: u128,
    /// Upper bound of the sanity band, at [`crate::scale::PRICE_SCALE`].
    pub max_price: u128,
    /// The exponent recorded when this feed was qualified.
    pub expected_exponent: i32,
}

impl OracleGuards {
    /// Guards suitable for opening a position: tight.
    ///
    /// Thirty seconds and 100 bps of confidence. A trade that cannot be priced
    /// confidently simply does not happen.
    pub fn for_trading(min_price: u128, max_price: u128, expected_exponent: i32) -> Self {
        Self {
            max_age_seconds: 30,
            max_age_slots: 150,
            max_future_skew_seconds: 5,
            max_confidence_bps: 100,
            min_price,
            max_price,
            expected_exponent,
        }
    }

    /// Guards suitable for liquidation: deliberately looser.
    ///
    /// Not because liquidation deserves less care, but because refusing to
    /// liquidate is not a safe default. A position that cannot be closed while
    /// the oracle is briefly degraded keeps losing money, and the pool absorbs
    /// it. Drift draws the same distinction, blocking fills at a far tighter
    /// divergence than it blocks liquidations.
    ///
    /// This is emphatically *not* a licence to liquidate on a bad price: the
    /// same structural checks apply, only the thresholds differ.
    pub fn for_liquidation(min_price: u128, max_price: u128, expected_exponent: i32) -> Self {
        Self {
            max_age_seconds: 60,
            max_age_slots: 300,
            max_future_skew_seconds: 5,
            max_confidence_bps: 500,
            min_price,
            max_price,
            expected_exponent,
        }
    }
}

/// A price that has passed every check, normalised to [`crate::scale::PRICE_SCALE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedPrice {
    /// Price at [`crate::scale::PRICE_SCALE`]. Always strictly positive.
    pub price: u128,
    /// Confidence interval at [`crate::scale::PRICE_SCALE`].
    pub confidence: u128,
    /// Upstream publish time, carried through for event emission.
    pub publish_time: i64,
    /// Slot the price landed on chain, carried through for event emission.
    pub posted_slot: u64,
}

/// Convert a mantissa and decimal exponent to [`crate::scale::PRICE_SCALE`].
///
/// # Rounding
///
/// Truncates. Unlike fees and margin there is no "pool's favour" direction for a
/// mark price: it values longs and shorts symmetrically, so rounding it in
/// either direction advantages one side and disadvantages the other. Truncation
/// is chosen for determinism, and the error is bounded by one unit at `1e10`
/// scale — around `1e-10` relative, far below the confidence interval of any
/// real feed.
pub fn normalize_price(mantissa: i64, exponent: i32) -> Result<u128, RiskError> {
    if mantissa <= 0 {
        return Err(RiskError::InvalidPrice);
    }
    normalize_magnitude(mantissa as u128, exponent)
}

/// As [`normalize_price`], for an already-unsigned quantity such as a confidence
/// interval.
pub fn normalize_magnitude(magnitude: u128, exponent: i32) -> Result<u128, RiskError> {
    if !(MIN_EXPONENT..=MAX_EXPONENT).contains(&exponent) {
        return Err(RiskError::UnexpectedExponent);
    }

    let shift = exponent
        .checked_add(PRICE_SCALE_EXPONENT)
        .ok_or(RiskError::MathOverflow)?;

    if shift >= 0 {
        let factor = pow10(shift as u32)?;
        magnitude.checked_mul(factor).ok_or(RiskError::MathOverflow)
    } else {
        // `shift` is bounded below by MIN_EXPONENT + PRICE_SCALE_EXPONENT, so
        // the negation cannot overflow.
        let factor = pow10(shift.unsigned_abs())?;
        Ok(magnitude / factor)
    }
}

/// Validate a raw oracle price against a market's guards.
///
/// Fails closed: any doubt produces an error rather than a price. The order of
/// checks is chosen so the cheapest and most fundamental run first, and so an
/// error message identifies the actual problem rather than a downstream symptom.
pub fn validate_price(
    raw: RawPrice,
    guards: &OracleGuards,
    now_unix: i64,
    now_slot: u64,
) -> Result<ValidatedPrice, RiskError> {
    // The exponent is checked before anything is scaled by it. A feed that has
    // changed exponent produces prices wrong by a power of ten, which would sail
    // through a sanity band chosen for the old scale.
    if raw.exponent != guards.expected_exponent {
        return Err(RiskError::UnexpectedExponent);
    }

    // A publish time in the future is rejected before any age arithmetic, so a
    // far-future timestamp cannot turn the staleness check into a permanent
    // bypass by making the elapsed time negative.
    if raw.publish_time > now_unix {
        match raw.publish_time.checked_sub(now_unix) {
            // A skew too large to fit in an i64 is emphatically in the future.
            // Reachable only with absurd inputs. The alternative, `saturating_sub`,
            // happens to clamp in the safe direction here — but relying on that
            // means reasoning about clamp direction case by case, which is
            // precisely what the no-saturating rule exists to avoid.
            None => return Err(RiskError::PriceFromTheFuture),
            Some(skew) if skew > guards.max_future_skew_seconds as i64 => {
                return Err(RiskError::PriceFromTheFuture)
            }
            Some(_) => {}
        }
    } else {
        let age = now_unix
            .checked_sub(raw.publish_time)
            .ok_or(RiskError::MathOverflow)?;
        if age > guards.max_age_seconds as i64 {
            return Err(RiskError::StalePrice);
        }
    }

    // Second clock: how long ago it landed here, regardless of what the upstream
    // timestamp claims.
    if now_slot > raw.posted_slot {
        let slots_elapsed = now_slot - raw.posted_slot;
        if slots_elapsed > guards.max_age_slots {
            return Err(RiskError::StalePrice);
        }
    }

    let price = normalize_price(raw.mantissa, raw.exponent)?;
    if price == 0 {
        return Err(RiskError::InvalidPrice);
    }

    let confidence = normalize_magnitude(raw.confidence as u128, raw.exponent)?;

    // Confidence as a fraction of price. Both sides are at PRICE_SCALE, so the
    // scales cancel and this is an exact comparison with no division.
    let confidence_limit = price
        .checked_mul(guards.max_confidence_bps as u128)
        .ok_or(RiskError::MathOverflow)?;
    let confidence_scaled = confidence
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(RiskError::MathOverflow)?;
    if confidence_scaled > confidence_limit {
        return Err(RiskError::ConfidenceTooWide);
    }

    // Sanity band last: by here the price is known well-formed, so a band
    // failure genuinely means the value is implausible rather than malformed.
    if price < guards.min_price || price > guards.max_price {
        return Err(RiskError::PriceOutOfBand);
    }

    Ok(ValidatedPrice {
        price,
        confidence,
        publish_time: raw.publish_time,
        posted_slot: raw.posted_slot,
    })
}

/// Whether a price diverges from a reference by more than the permitted amount.
///
/// Used to gate opening a position against a longer-horizon average: a spot
/// price far from its own recent history is exactly what oracle manipulation
/// looks like from the inside. Applied to opens rather than to liquidations,
/// for the reason given on [`OracleGuards::for_liquidation`].
pub fn diverges_beyond(
    price: u128,
    reference: u128,
    max_divergence_bps: u16,
) -> Result<bool, RiskError> {
    if reference == 0 {
        return Err(RiskError::InvalidPrice);
    }
    let gap = price.abs_diff(reference);
    let scaled = gap
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(RiskError::MathOverflow)?;
    let limit = reference
        .checked_mul(max_divergence_bps as u128)
        .ok_or(RiskError::MathOverflow)?;
    Ok(scaled > limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scale::PRICE_SCALE;

    fn guards() -> OracleGuards {
        OracleGuards::for_trading(1, u128::MAX, -8)
    }

    fn fresh(mantissa: i64, confidence: u64) -> RawPrice {
        RawPrice {
            mantissa,
            confidence,
            exponent: -8,
            publish_time: 1_000_000,
            posted_slot: 500,
        }
    }

    #[test]
    fn a_pyth_crypto_price_normalises_correctly() {
        // $123.45 at exponent -8 is 12_345_000_000.
        assert_eq!(
            normalize_price(12_345_000_000, -8).unwrap(),
            12_345 * PRICE_SCALE / 100
        );
    }

    #[test]
    fn a_sub_cent_price_keeps_significant_figures() {
        // $0.0002823 at exponent -8. The reason PRICE_SCALE is 1e10 and not 1e6.
        let price = normalize_price(28_230, -8).unwrap();
        assert_eq!(price, 2_823_000);
        assert!(price > 0, "a sub-cent price must not truncate to zero");
    }

    #[test]
    fn a_negative_or_zero_mantissa_is_rejected() {
        assert_eq!(normalize_price(-1, -8), Err(RiskError::InvalidPrice));
        assert_eq!(normalize_price(0, -8), Err(RiskError::InvalidPrice));
    }

    #[test]
    fn an_out_of_band_exponent_is_rejected_rather_than_panicking() {
        assert_eq!(normalize_price(1, 1), Err(RiskError::UnexpectedExponent));
        assert_eq!(normalize_price(1, -19), Err(RiskError::UnexpectedExponent));
        assert_eq!(
            normalize_price(1, i32::MIN),
            Err(RiskError::UnexpectedExponent)
        );
        assert_eq!(
            normalize_price(1, i32::MAX),
            Err(RiskError::UnexpectedExponent)
        );
    }

    #[test]
    fn a_valid_price_passes() {
        let result = validate_price(fresh(12_345_000_000, 1_000_000), &guards(), 1_000_010, 520);
        assert!(result.is_ok(), "expected a valid price, got {result:?}");
    }

    #[test]
    fn a_stale_price_is_rejected() {
        // Published 31 seconds ago against a 30 second limit.
        assert_eq!(
            validate_price(fresh(12_345_000_000, 0), &guards(), 1_000_031, 520),
            Err(RiskError::StalePrice)
        );
    }

    #[test]
    fn a_price_stale_by_slots_is_rejected_even_when_its_timestamp_looks_fresh() {
        // Timestamp is current, but it landed 200 slots ago against a 150 limit.
        // This is the case a single-clock check misses entirely.
        assert_eq!(
            validate_price(fresh(12_345_000_000, 0), &guards(), 1_000_000, 700),
            Err(RiskError::StalePrice)
        );
    }

    #[test]
    fn a_future_timestamp_cannot_bypass_the_staleness_check() {
        // The critical case. A naive `now - publish_time > max_age` test would
        // compute a negative age here and accept this price forever.
        let mut raw = fresh(12_345_000_000, 0);
        raw.publish_time = 2_000_000;
        assert_eq!(
            validate_price(raw, &guards(), 1_000_000, 520),
            Err(RiskError::PriceFromTheFuture)
        );
    }

    #[test]
    fn small_clock_skew_into_the_future_is_tolerated() {
        let mut raw = fresh(12_345_000_000, 0);
        raw.publish_time = 1_000_003;
        assert!(validate_price(raw, &guards(), 1_000_000, 520).is_ok());
    }

    #[test]
    fn a_wide_confidence_interval_is_rejected() {
        // Confidence of 2% against a 1% limit.
        let raw = fresh(12_345_000_000, 246_900_000);
        assert_eq!(
            validate_price(raw, &guards(), 1_000_000, 520),
            Err(RiskError::ConfidenceTooWide)
        );
    }

    #[test]
    fn liquidation_guards_accept_what_trading_guards_reject() {
        // The same degraded price: too uncertain to open against, but a
        // liquidation must still be able to proceed.
        let raw = fresh(12_345_000_000, 246_900_000);
        let trading = OracleGuards::for_trading(1, u128::MAX, -8);
        let liquidation = OracleGuards::for_liquidation(1, u128::MAX, -8);

        assert_eq!(
            validate_price(raw, &trading, 1_000_000, 520),
            Err(RiskError::ConfidenceTooWide)
        );
        assert!(validate_price(raw, &liquidation, 1_000_000, 520).is_ok());
    }

    #[test]
    fn a_changed_exponent_is_rejected_before_anything_is_scaled_by_it() {
        let mut raw = fresh(12_345_000_000, 0);
        raw.exponent = -6;
        assert_eq!(
            validate_price(raw, &guards(), 1_000_000, 520),
            Err(RiskError::UnexpectedExponent)
        );
    }

    #[test]
    fn a_price_outside_the_sanity_band_is_rejected() {
        let mut g = guards();
        g.min_price = 100 * PRICE_SCALE;
        g.max_price = 200 * PRICE_SCALE;
        // $123.45 is inside.
        assert!(validate_price(fresh(12_345_000_000, 0), &g, 1_000_000, 520).is_ok());
        // $1.23 is not.
        assert_eq!(
            validate_price(fresh(123_000_000, 0), &g, 1_000_000, 520),
            Err(RiskError::PriceOutOfBand)
        );
    }

    #[test]
    fn divergence_is_measured_against_the_reference() {
        let reference = 100 * PRICE_SCALE;
        assert!(!diverges_beyond(105 * PRICE_SCALE, reference, 1_000).unwrap());
        assert!(diverges_beyond(120 * PRICE_SCALE, reference, 1_000).unwrap());
        // Symmetric: a crash diverges as much as a spike.
        assert!(diverges_beyond(80 * PRICE_SCALE, reference, 1_000).unwrap());
    }
}
