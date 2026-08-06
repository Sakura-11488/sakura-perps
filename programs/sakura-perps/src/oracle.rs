//! Reading a Pyth price feed and handing it to the risk crate for validation.
//!
//! This module does exactly two things: deserialise a `PriceUpdateV2` account,
//! and translate errors. Every judgement about whether a price is safe to trade
//! on lives in [`sakura_perps_risk::oracle`], which has no Solana dependency and
//! is therefore testable exhaustively in milliseconds.
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
use sakura_perps_risk::oracle::{validate_price, OracleGuards, RawPrice, ValidatedPrice};
use sakura_perps_risk::RiskError;

use crate::PerpsError;

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
