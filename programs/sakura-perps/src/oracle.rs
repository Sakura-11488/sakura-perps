//! Reading a Pyth price feed and handing it to the risk crate for validation.
//!
//! Deserializes a `PriceUpdateV2` account, translates validation errors, and
//! records strict cache provenance. Numeric oracle guards live in
//! [`sakura_perps_risk::oracle`], which has no Solana dependency.
//!
//! # Why there is no mock oracle behind a feature flag
//!
//! The obvious design is a `PriceSource` trait with a real Pyth implementation
//! and a mock one for tests. It is unnecessary here, and worse than the
//! alternative. Because validation is a pure function over five numbers, the
//! interesting cases — a stale price, a future timestamp, a widened confidence
//! interval, a changed exponent — are already covered by property tests that run
//! in milliseconds with no runtime at all.
//!
//! What remains untested by those is this file: whether a real account
//! deserialises into the right five numbers. A mock cannot test that, by
//! definition. LiteSVM can, by writing genuine `PriceUpdateV2` bytes into an
//! account, which is how the integration tests exercise it. So a mock would add
//! an abstraction layer that tests nothing the pure functions do not already
//! cover, while making the on-chain path *less* like production.
//!
//! # The two ages, and why the SDK's check is not enough on its own
//!
//! [`PriceUpdateV2::get_price_no_older_than`] enforces three things, and they
//! are all worth having: full Wormhole verification, an upper bound on age, and
//! that the update actually belongs to the feed being asked for. That last one
//! is what stops someone presenting a BONK price to a SOL market.
//!
//! It does not check `posted_slot`. A price can carry a perfectly fresh upstream
//! timestamp while having sat on this chain for a long time, so the risk crate
//! checks the slot separately. Nor does it check the confidence interval, the
//! exponent, or a sanity band, none of which the SDK has any opinion about.

use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;
use sakura_perps_risk::oracle::{
    normalize_price, validate_price, OracleGuards, RawPrice, ValidatedPrice,
};
use sakura_perps_risk::RiskError;

use crate::{market::Market, PerpsError};

/// Cache format that records a raw spot accepted under the trading guards.
/// Zero is legacy state with no evidence of which guards or clock were used.
pub(crate) const STRICT_PRICE_SOURCE_VERSION: u8 = 1;

/// Preserve the newest strict spot and its upstream publication time while the
/// market is active. Quarantine freezes this common emergency reference: exits
/// cannot select a different terminal price by refreshing between settlements.
///
/// Call only after `load_price` succeeds with this market's trading guards.
/// Liquidation's looser guards and an EMA-derived execution price are not cache
/// provenance. An old/equal observation is a harmless no-op, never a reason to
/// reject an otherwise valid close. The first strict write replaces legacy
/// state without comparing its old transaction timestamp to a publish time.
pub(crate) fn record_strict_price(market: &mut Market, price: &ValidatedPrice) -> bool {
    if market.is_quarantined()
        || !matches!(
            market.last_good_price_source,
            0 | STRICT_PRICE_SOURCE_VERSION
        )
        || (market.last_good_price_source == STRICT_PRICE_SOURCE_VERSION
            && price.publish_time <= market.last_good_price_ts)
    {
        return false;
    }
    market.last_good_price = price.price;
    market.last_good_price_ts = price.publish_time;
    market.last_good_price_source = STRICT_PRICE_SOURCE_VERSION;
    true
}

/// Read and fully validate a price from a Pyth `PriceUpdateV2` account.
///
/// `feed_id` must come from the market's own configuration, never from
/// instruction data. Letting a caller supply it would make the feed-id check
/// inside the SDK meaningless — they would simply pass the id matching whatever
/// account they handed over.
pub fn load_price(
    price_update: &Account<'_, PriceUpdateV2>,
    feed_id: &[u8; 32],
    guards: &OracleGuards,
    clock: &Clock,
) -> Result<ValidatedPrice> {
    // Never `get_price_unchecked`, whose own documentation warns that it skips
    // both recency and verification. CI greps for that name across the whole
    // repository so it cannot creep back in.
    //
    // The SDK is given the same age bound the risk crate will apply, so the two
    // agree rather than one silently permitting what the other forbids.
    let price = price_update
        .get_price_no_older_than(clock, guards.max_age_seconds as u64, feed_id)
        .map_err(|_| error!(PerpsError::OraclePriceUnavailable))?;

    let raw = RawPrice {
        mantissa: price.price,
        confidence: price.conf,
        exponent: price.exponent,
        publish_time: price.publish_time,
        // Not covered by the SDK's own recency check: this is when the update
        // landed here, as opposed to when the upstream published it.
        posted_slot: price_update.posted_slot,
    };

    validate_price(raw, guards, clock.unix_timestamp, clock.slot).map_err(map_risk_error)
}

/// [`load_price`], plus the feed's EMA price at `PRICE_SCALE`.
///
/// The EMA is the reference the divergence check needs: a spot price far from
/// its own recent history is what oracle manipulation looks like from the
/// inside, and [`load_price`] returns no such reference, so every caller that
/// wanted one had nothing to compare against.
///
/// # There is no second exponent to cross-check
///
/// An earlier design required this function to reject the update when the spot
/// and EMA exponents disagreed. They cannot: `PriceFeedMessage` carries a
/// **single** `exponent: i32`, shared by `price`/`conf` and `ema_price`/
/// `ema_conf`. The check was unimplementable, not merely redundant.
///
/// What replaces it is stronger. That one exponent is validated against the
/// market's `expected_exponent` exactly once — [`validate_price`] already does
/// it for the spot leg — and both mantissas are then normalised through it. The
/// two are commensurable **by construction** rather than by comparison, so
/// there is no state in which they could disagree and no check to forget.
///
/// `ema_conf` is not used. The EMA influences the existing exit-price clamp,
/// so it remains an economic oracle assumption, not independent evidence that
/// the clamped price is fair. Only the validated raw spot can enter the strict
/// emergency cache; the clamp's synthetic midpoint cannot.
///
/// # A broken EMA returns zero rather than an error
///
/// A nonpositive EMA has no usable reference. Exits pass the already validated
/// spot through `clamp_to_ema_band` unchanged in this case; new entries fail
/// closed because `diverges_beyond` rejects a zero reference. Strict cache refresh
/// calls `load_price` directly and does not depend on the EMA at all.
pub fn load_price_and_ema(
    price_update: &Account<'_, PriceUpdateV2>,
    feed_id: &[u8; 32],
    guards: &OracleGuards,
    clock: &Clock,
) -> Result<(ValidatedPrice, u128)> {
    // Runs first, so nothing below is reached until the feed id, the exponent,
    // both clocks, the confidence interval and the sanity band have all passed.
    let price = load_price(price_update, feed_id, guards, clock)?;

    // The mantissa is tested rather than the error swallowed, so an
    // `UnexpectedExponent` — a genuinely different failure — would still
    // propagate. It cannot arise here in practice: `load_price` has already
    // matched this same exponent against the market's expected one, and
    // `qualify_feed` bounds that to the range `normalize_price` accepts.
    let ema = if price_update.price_message.ema_price > 0 {
        normalize_price(
            price_update.price_message.ema_price,
            price_update.price_message.exponent,
        )
        .map_err(map_risk_error)?
    } else {
        0
    };

    Ok((price, ema))
}

/// Translate a [`RiskError`] into the program's error space.
///
/// Each oracle rejection keeps its own variant rather than collapsing into a
/// generic failure. An operator debugging a market that has stopped trading
/// needs to know whether the feed is stale, has widened, or has drifted outside
/// its sanity band — those have completely different responses, and a single
/// `OracleError` would hide the distinction at exactly the wrong moment.
pub fn map_risk_error(err: RiskError) -> Error {
    match err {
        RiskError::StalePrice => error!(PerpsError::OracleStale),
        RiskError::ConfidenceTooWide => error!(PerpsError::OracleConfidenceTooWide),
        RiskError::PriceOutOfBand => error!(PerpsError::OraclePriceOutOfBand),
        RiskError::UnexpectedExponent => error!(PerpsError::OracleExponentChanged),
        RiskError::PriceFromTheFuture => error!(PerpsError::OraclePriceFromTheFuture),
        RiskError::InvalidPrice => error!(PerpsError::OracleInvalidPrice),
        RiskError::DivisionByZero | RiskError::MathOverflow => error!(PerpsError::MathOverflow),
        RiskError::NegativeAmount => error!(PerpsError::NegativeAmount),
        RiskError::ZeroSize => error!(PerpsError::ZeroSize),
        RiskError::InvalidBasisPoints => error!(PerpsError::InvalidBasisPoints),
        RiskError::EmptyPool => error!(PerpsError::EmptyPool),
        RiskError::InvalidMarginParameters => error!(PerpsError::InvalidMarginParameters),
        RiskError::GuardsNotOrdered => error!(PerpsError::GuardsNotOrdered),
    }
}

/// Guards for opening a position, built from a market's stored configuration.
pub fn trading_guards(min_price: u128, max_price: u128, expected_exponent: i32) -> OracleGuards {
    OracleGuards::for_trading(min_price, max_price, expected_exponent)
}

/// Guards for liquidating a position: deliberately looser than [`trading_guards`].
///
/// Refusing to liquidate is not a safe default. A position that cannot be closed
/// while the oracle is briefly degraded keeps losing money, and the pool absorbs
/// the difference.
pub fn liquidation_guards(
    min_price: u128,
    max_price: u128,
    expected_exponent: i32,
) -> OracleGuards {
    OracleGuards::for_liquidation(min_price, max_price, expected_exponent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::tests::test_market;

    fn quote(price: u128, publish_time: i64) -> ValidatedPrice {
        ValidatedPrice {
            price,
            confidence: 1,
            publish_time,
            posted_slot: 10,
        }
    }

    #[test]
    fn strict_cache_uses_publication_time_and_never_regresses() {
        let mut market = test_market();
        // Legacy transaction time is not comparable to the new publication time.
        market.last_good_price = 99;
        market.last_good_price_ts = 1_000;
        assert!(record_strict_price(&mut market, &quote(100, 900)));
        assert_eq!(market.last_good_price_source, STRICT_PRICE_SOURCE_VERSION);
        assert_eq!(
            (market.last_good_price, market.last_good_price_ts),
            (100, 900)
        );
        for stale in [quote(200, 899), quote(300, 900)] {
            assert!(!record_strict_price(&mut market, &stale));
            assert_eq!(
                (market.last_good_price, market.last_good_price_ts),
                (100, 900)
            );
        }
        assert!(record_strict_price(&mut market, &quote(400, 901)));
        assert_eq!(
            (market.last_good_price, market.last_good_price_ts),
            (400, 901)
        );
    }

    #[test]
    fn quarantine_freezes_the_reference_and_cannot_initialize_legacy_provenance() {
        let mut market = test_market();
        assert!(record_strict_price(&mut market, &quote(100, 900)));
        market.max_oi_usd = 0;
        assert!(!record_strict_price(&mut market, &quote(200, 901)));
        assert_eq!(
            (market.last_good_price, market.last_good_price_ts),
            (100, 900)
        );
        market.last_good_price_source = 0;
        assert!(!record_strict_price(&mut market, &quote(300, 902)));
        assert_eq!(market.last_good_price_source, 0);
        market.max_oi_usd = 1;
        assert!(record_strict_price(&mut market, &quote(300, 902)));
        assert_eq!(market.last_good_price_source, STRICT_PRICE_SOURCE_VERSION);
    }
}
