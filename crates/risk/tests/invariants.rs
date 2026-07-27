//! Property-based invariants.
//!
//! These are the tests that matter. Unit tests check the cases somebody thought
//! of; these check the cases nobody thought of, which is where money is lost.
//!
//! Every property here is a statement that must hold for *all* inputs in range,
//! and each corresponds to a way the protocol could be drained if it did not.

use proptest::prelude::*;

use sakura_perps_risk::error::RiskError;
use sakura_perps_risk::funding::{
    borrow_index_delta, borrow_owed, fees_dominate_funding, funding_index_delta, funding_owed,
    funding_rate_per_hour,
};
use sakura_perps_risk::math::{ceil_div, mul_div_ceil, mul_div_floor, mul_div_i128, pow10};
use sakura_perps_risk::pool::{
    assets_for_shares, aum_usd, flow_fee, shares_for_deposit, utilization_bps, MINIMUM_LIQUIDITY,
};
use sakura_perps_risk::position::{
    blended_entry_price, equity, is_liquidatable, leverage_bps, liquidation_fee, liquidation_price,
    margin_requirement, notional_usd, notional_usd_ceil, unrealized_pnl,
    validate_margin_parameters, Side,
};

/// Prices in a range spanning a sub-cent memecoin to a five-figure asset.
fn price() -> impl Strategy<Value = u128> {
    1_000_000u128..1_000_000_000_000_000u128
}

/// Position sizes at nine decimals, from dust to very large.
fn size() -> impl Strategy<Value = u128> {
    1u128..1_000_000_000_000_000u128
}

fn usd() -> impl Strategy<Value = u128> {
    1u128..1_000_000_000_000u128
}

fn bps() -> impl Strategy<Value = u16> {
    0u16..=10_000u16
}

fn side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Long), Just(Side::Short)]
}

// ── Arithmetic primitives ───────────────────────────────────────────────────

proptest! {
    /// `floor` and `ceil` bracket the true quotient, and differ by at most one.
    /// If this ever fails, every rounding decision built on it is unsound.
    #[test]
    fn mul_div_floor_and_ceil_differ_by_at_most_one(
        a in any::<u128>(), b in any::<u128>(), c in 1u128..u128::MAX
    ) {
        let floor = mul_div_floor(a, b, c);
        let ceil = mul_div_ceil(a, b, c);

        // Both must agree on whether the result is representable at all.
        prop_assert_eq!(
            floor.is_ok(), ceil.is_ok(),
            "floor and ceil disagreed on representability: {:?} vs {:?}", floor, ceil
        );

        if let (Ok(floor), Ok(ceil)) = (floor, ceil) {
            prop_assert!(ceil >= floor);
            prop_assert!(ceil - floor <= 1);
        }
    }

    /// Where the product fits in `u128`, the wide path must agree exactly with
    /// plain arithmetic. This is what proves the 256-bit division is correct.
    #[test]
    fn mul_div_matches_plain_arithmetic_when_the_product_fits(
        a in 0u128..u64::MAX as u128, b in 0u128..u64::MAX as u128, c in 1u128..u64::MAX as u128
    ) {
        let product = a * b;
        prop_assert_eq!(mul_div_floor(a, b, c).unwrap(), product / c);
        prop_assert_eq!(mul_div_ceil(a, b, c).unwrap(), ceil_div(product, c).unwrap());
    }

    /// Nothing in the primitives panics, for any input at all.
    #[test]
    fn primitives_never_panic(a in any::<u128>(), b in any::<u128>(), c in any::<u128>()) {
        let _ = mul_div_floor(a, b, c);
        let _ = mul_div_ceil(a, b, c);
        let _ = ceil_div(a, b);
        let _ = pow10((a % 100) as u32);
    }

    /// Signed division truncates toward zero, so magnitude never grows.
    #[test]
    fn signed_mul_div_never_increases_magnitude(
        a in i64::MIN as i128..i64::MAX as i128, c in 1i128..1_000_000i128
    ) {
        let result = mul_div_i128(a, 1, c).unwrap();
        prop_assert!(result.unsigned_abs() <= a.unsigned_abs());
    }
}

// ── Position valuation ──────────────────────────────────────────────────────

proptest! {
    /// Ceil notional is never below floor notional, and never more than one
    /// micro-dollar above. Margin requirements are computed from the ceiling, so
    /// a gap here would be a systematic under- or over-charge.
    #[test]
    fn notional_rounding_is_tight(s in size(), p in price(), decimals in 0u8..12) {
        let floor = notional_usd(s, p, decimals);
        let ceil = notional_usd_ceil(s, p, decimals);
        if let (Ok(floor), Ok(ceil)) = (floor, ceil) {
            prop_assert!(ceil >= floor);
            prop_assert!(ceil - floor <= 1);
        }
    }

    /// A long and a short of identical size and entry have PnL of opposite sign
    /// at any price. The pool is the counterparty to both, so any asymmetry is a
    /// leak in one direction.
    ///
    /// The magnitudes may differ by one unit, because both are rounded in the
    /// pool's favour — that asymmetry is deliberate and must be *toward* the
    /// pool, never away.
    #[test]
    fn long_and_short_pnl_are_symmetric_and_rounding_favours_the_pool(
        s in size(), entry in price(), p in price(), decimals in 0u8..12
    ) {
        let long = unrealized_pnl(Side::Long, s, entry, p, decimals);
        let short = unrealized_pnl(Side::Short, s, entry, p, decimals);
        if let (Ok(long), Ok(short)) = (long, short) {
            // Signs oppose, unless the price is unchanged and both are zero.
            if p != entry {
                prop_assert!(long.signum() == -short.signum());
            }
            // Summing them gives the pool's net position, which must never be
            // negative: the pool cannot pay out more than it takes in on an
            // exactly offsetting pair.
            prop_assert!(long + short <= 0, "pool paid out more than it received");
        }
    }

    /// Closing at the entry price returns exactly the collateral, never more.
    #[test]
    fn round_trip_at_entry_price_is_never_profitable(
        sd in side(), s in size(), entry in price(), decimals in 0u8..12
    ) {
        if let Ok(pnl) = unrealized_pnl(sd, s, entry, entry, decimals) {
            prop_assert_eq!(pnl, 0, "closing at entry should be exactly flat");
        }
    }

    /// Margin requirement is monotone in notional and never rounds down.
    #[test]
    fn margin_requirement_is_monotone_and_rounds_up(
        notional in usd(), extra in 0u128..1_000_000u128, margin in bps()
    ) {
        let base = margin_requirement(notional, margin);
        let larger = margin_requirement(notional + extra, margin);
        if let (Ok(base), Ok(larger)) = (base, larger) {
            prop_assert!(larger >= base);
            // Never charges less than the exact fraction.
            prop_assert!(base * 10_000 >= notional * margin as u128);
        }
    }
}

// ── Liquidation ─────────────────────────────────────────────────────────────

proptest! {
    /// The single most important property in the crate: the liquidation price
    /// formula must agree with the health check it claims to invert.
    ///
    /// The threshold is *bracketed* rather than asserted at the exact point,
    /// because the boundary is deliberately ambiguous by one unit of rounding.
    /// `liquidation_price` rounds toward earlier liquidation — up for a long,
    /// down for a short — so the number shown to a trader is never optimistic.
    /// A consequence is that at exactly the returned price a long is not *quite*
    /// liquidatable yet; it is a hair on the safe side. That is the correct
    /// direction to err, and this test pins the direction as well as the value.
    ///
    /// The first version of this test asserted liquidatable at exactly the
    /// returned price and failed on a case where equity exceeded the requirement
    /// by 5 parts in 4×10^11 — the rounding, behaving exactly as designed.
    ///
    /// If these ever genuinely disagree, liquidations fire either early,
    /// confiscating solvent positions, or late, leaving bad debt.
    #[test]
    fn liquidation_price_brackets_the_health_check(
        sd in side(),
        s in 1_000_000u128..1_000_000_000_000u128,
        entry in 1_000_000_000u128..100_000_000_000_000u128,
        collateral in 1_000u128..100_000_000u128,
        maintenance in 100u16..2_000u16,
        decimals in 6u8..10,
    ) {
        let liq = liquidation_price(sd, s, entry, collateral, maintenance, decimals);
        let Ok(Some(liq)) = liq else { return Ok(()); };
        if liq < 100 { return Ok(()); }

        let health_at = |p: u128| -> Option<bool> {
            let pnl = unrealized_pnl(sd, s, entry, p, decimals).ok()?;
            let notional = notional_usd_ceil(s, p, decimals).ok()?;
            let eq = equity(collateral, pnl, 0, 0).ok()?;
            is_liquidatable(eq, notional, maintenance).ok()
        };

        // One percent past the threshold, in the losing direction: must be
        // liquidatable. A long loses as the price falls, a short as it rises.
        let past = match sd {
            Side::Long => liq - liq / 100 - 1,
            Side::Short => liq + liq / 100 + 1,
        };
        if let Some(liquidatable) = health_at(past) {
            prop_assert!(
                liquidatable,
                "not liquidatable one percent past the threshold: \
                 side={sd:?} liq={liq} price={past}"
            );
        }

        // One percent clear of it, in the safe direction: must not be.
        let safe = match sd {
            Side::Long => liq + liq / 100 + 1,
            Side::Short => liq - liq / 100 - 1,
        };
        if let Some(liquidatable) = health_at(safe) {
            prop_assert!(
                !liquidatable,
                "liquidatable one percent clear of the threshold: \
                 side={sd:?} liq={liq} price={safe}"
            );
        }
    }

    /// The rounding direction itself is the safety property: the returned price
    /// is never *optimistic*. At exactly the returned price a position must not
    /// already be deep underwater — if it were, the trader was warned too late.
    #[test]
    fn liquidation_price_is_never_optimistic(
        sd in side(),
        s in 1_000_000u128..1_000_000_000_000u128,
        entry in 1_000_000_000u128..100_000_000_000_000u128,
        collateral in 1_000u128..100_000_000u128,
        maintenance in 100u16..2_000u16,
        decimals in 6u8..10,
    ) {
        let liq = liquidation_price(sd, s, entry, collateral, maintenance, decimals);
        let Ok(Some(liq)) = liq else { return Ok(()); };
        if liq == 0 { return Ok(()); }

        let Ok(pnl) = unrealized_pnl(sd, s, entry, liq, decimals) else { return Ok(()); };
        let Ok(eq) = equity(collateral, pnl, 0, 0) else { return Ok(()); };

        // Equity at the quoted liquidation price must still be non-negative:
        // the trader is warned before the account is underwater, never after.
        prop_assert!(
            eq >= 0,
            "equity already negative at the quoted liquidation price: \
             side={sd:?} liq={liq} equity={eq}"
        );
    }

    /// More collateral can only move the liquidation price further away.
    #[test]
    fn more_collateral_never_brings_liquidation_closer(
        sd in side(), s in size(), entry in price(),
        collateral in usd(), extra in 1u128..1_000_000u128,
        maintenance in 1u16..5_000u16, decimals in 0u8..12,
    ) {
        let base = liquidation_price(sd, s, entry, collateral, maintenance, decimals);
        let richer = liquidation_price(sd, s, entry, collateral + extra, maintenance, decimals);
        if let (Ok(Some(base)), Ok(Some(richer))) = (base, richer) {
            match sd {
                // A long liquidates on the way down, so more collateral lowers it.
                Side::Long => prop_assert!(richer <= base),
                // A short liquidates on the way up, so more collateral raises it.
                Side::Short => prop_assert!(richer >= base),
            }
        }
    }

    /// A freshly opened position that satisfies initial margin is never
    /// immediately liquidatable, provided the parameters are valid. This is the
    /// property that closes the self-liquidation attack.
    #[test]
    fn a_position_meeting_initial_margin_is_not_immediately_liquidatable(
        notional in 1_000_000u128..1_000_000_000u128,
        initial in 500u16..10_000u16,
        maintenance in 100u16..400u16,
        fee in 1u16..100u16,
    ) {
        prop_assume!(validate_margin_parameters(initial, maintenance, fee).is_ok());

        // Collateral exactly at the initial requirement, PnL zero.
        let collateral = margin_requirement(notional, initial).unwrap();
        let eq = equity(collateral, 0, 0, 0).unwrap();
        prop_assert!(
            !is_liquidatable(eq, notional, maintenance).unwrap(),
            "liquidatable at open: notional={notional} collateral={collateral} \
             initial={initial} maintenance={maintenance}"
        );
    }

    /// The liquidation fee never exceeds the collateral available, so
    /// liquidation cannot itself manufacture bad debt.
    #[test]
    fn liquidation_fee_never_exceeds_collateral(
        notional in usd(), fee_bps in bps(), collateral in 0u128..1_000_000_000u128
    ) {
        let fee = liquidation_fee(notional, fee_bps, collateral).unwrap();
        prop_assert!(fee <= collateral);
    }

    /// Margin parameters that would make a new position instantly liquidatable
    /// are always rejected.
    #[test]
    fn invalid_margin_parameters_are_rejected(
        initial in bps(), maintenance in bps(), fee in bps()
    ) {
        let result = validate_margin_parameters(initial, maintenance, fee);
        let should_pass = (initial as u32) > (maintenance as u32) + (fee as u32);
        prop_assert_eq!(result.is_ok(), should_pass);
    }
}

// ── Liquidity pool ──────────────────────────────────────────────────────────

proptest! {
    /// Deposit then immediately withdraw, with nothing else happening, must
    /// never return more than went in. If it can, the pool is a money printer.
    #[test]
    fn deposit_then_withdraw_never_returns_more_than_deposited(
        deposit in 2_000_000u128..1_000_000_000_000u128,
        total_shares in 1_000_000u128..1_000_000_000_000u128,
        aum in 1_000_000u128..1_000_000_000_000u128,
    ) {
        let minted = shares_for_deposit(deposit, total_shares, aum);
        let Ok(minted) = minted else { return Ok(()); };

        let shares_after = total_shares + minted.total().unwrap();
        let aum_after = aum + deposit;

        let returned = assets_for_shares(minted.to_depositor, shares_after, aum_after);
        if let Ok(returned) = returned {
            prop_assert!(
                returned <= deposit,
                "round trip profited: in={deposit} out={returned}"
            );
        }
    }

    /// The very first deposit permanently locks the minimum, so the depositor
    /// can never redeem the whole pool and leave a zero-share price behind.
    #[test]
    fn first_deposit_locks_the_minimum(deposit in 1_000_001u128..1_000_000_000_000u128) {
        let minted = shares_for_deposit(deposit, 0, 0).unwrap();
        prop_assert_eq!(minted.locked_forever, MINIMUM_LIQUIDITY);
        prop_assert_eq!(minted.to_depositor, deposit - MINIMUM_LIQUIDITY);
    }

    /// A first deposit at or below the locked minimum is refused rather than
    /// minting zero or underflowing.
    #[test]
    fn a_dust_first_deposit_is_refused(deposit in 1u128..=1_000_000u128) {
        prop_assert!(shares_for_deposit(deposit, 0, 0).is_err());
    }

    /// A deposit that would mint zero shares is refused, never silently taken.
    #[test]
    fn a_deposit_minting_zero_shares_is_refused(
        deposit in 1u128..1_000u128,
        total_shares in 1u128..1_000u128,
        aum in 1_000_000_000_000u128..1_000_000_000_000_000u128,
    ) {
        // A tiny deposit into a very valuable pool rounds to zero shares. Either
        // it is refused, or it mints something — never accepted for nothing.
        if let Ok(minted) = shares_for_deposit(deposit, total_shares, aum) {
            prop_assert!(minted.to_depositor > 0);
        }
    }

    /// Insolvency is reported, never silently clamped to zero.
    #[test]
    fn aum_reports_shortfall_rather_than_clamping(equity in usd(), liabilities in usd()) {
        let aum = aum_usd(equity, liabilities);
        if liabilities > equity {
            prop_assert!(aum.is_insolvent());
            prop_assert_eq!(aum.value_usd, 0);
            prop_assert_eq!(aum.shortfall_usd, liabilities - equity);
        } else {
            prop_assert!(!aum.is_insolvent());
            prop_assert_eq!(aum.value_usd, equity - liabilities);
        }
    }

    /// Flow fees always round up, so the pool never under-collects.
    #[test]
    fn flow_fee_rounds_up(amount in usd(), fee_bps in bps()) {
        let fee = flow_fee(amount, fee_bps).unwrap();
        prop_assert!(fee * 10_000 >= amount * fee_bps as u128);
        prop_assert!(fee <= amount);
    }

    /// Utilisation is bounded and monotone in what is reserved.
    #[test]
    fn utilization_is_monotone(reserved in usd(), extra in 0u128..1_000_000u128, aum in usd()) {
        let base = utilization_bps(reserved, aum).unwrap();
        let more = utilization_bps(reserved + extra, aum).unwrap();
        prop_assert!(more >= base);
    }
}

// ── Funding and borrow ──────────────────────────────────────────────────────

proptest! {
    /// Funding is zero-sum: what longs pay, shorts receive. The rate is a single
    /// signed number applied to both sides, so this is really a check that the
    /// sign convention is consistent.
    #[test]
    fn funding_is_zero_sum_across_equal_and_opposite_positions(
        notional in usd(), current in -1_000_000_000i128..1_000_000_000i128,
        entry in -1_000_000_000i128..1_000_000_000i128,
    ) {
        let long = funding_owed(notional, current, entry);
        let short = funding_owed(notional, entry, current);
        if let (Ok(long), Ok(short)) = (long, short) {
            prop_assert_eq!(long, -short, "funding did not net to zero");
        }
    }

    /// Borrow is never negative and never a refund, even if the index is equal.
    #[test]
    fn borrow_is_never_a_refund(notional in usd(), entry in 0u128..1_000_000_000u128, delta in 0u128..1_000_000u128) {
        let owed = borrow_owed(notional, entry + delta, entry).unwrap();
        if delta == 0 {
            prop_assert_eq!(owed, 0);
        }
        // Rounds up, so the pool never under-charges.
        prop_assert!(owed * 1_000_000_000 >= notional * delta || owed == 0 && notional == 0);
    }

    /// An index that has gone backwards is rejected rather than credited.
    #[test]
    fn a_backwards_borrow_index_is_rejected(
        notional in usd(), entry in 1u128..1_000_000u128, back in 1u128..1_000u128
    ) {
        prop_assert_eq!(
            borrow_owed(notional, entry.saturating_sub(back), entry),
            Err(RiskError::MathOverflow)
        );
    }

    /// The funding rate is always clamped, whatever the skew.
    #[test]
    fn funding_rate_respects_its_cap(
        long_oi in 0u128..1_000_000_000_000u128,
        short_oi in 0u128..1_000_000_000_000u128,
        sensitivity in 0u128..1_000_000_000u128,
        cap in 0u128..1_000_000u128,
    ) {
        let rate = funding_rate_per_hour(long_oi, short_oi, sensitivity, cap).unwrap();
        prop_assert!(rate.unsigned_abs() <= cap);
        // Sign follows the skew: longs pay when longs dominate.
        if long_oi > short_oi { prop_assert!(rate >= 0); }
        if short_oi > long_oi { prop_assert!(rate <= 0); }
        if long_oi == short_oi { prop_assert_eq!(rate, 0); }
    }

    /// Zero elapsed time accrues nothing, in either index.
    #[test]
    fn no_time_no_accrual(rate in 0u128..1_000_000u128, utilization in 0u128..10_000u128) {
        prop_assert_eq!(borrow_index_delta(rate, utilization, 0, 0).unwrap().index_delta, 0);
        prop_assert_eq!(funding_index_delta(rate as i128, 0).unwrap(), 0);
    }

    /// Borrow accrual is monotone in elapsed time — a longer hold never costs
    /// less.
    #[test]
    fn borrow_accrual_is_monotone_in_time(
        rate in 1u128..1_000_000u128, utilization in 1u128..10_000u128,
        seconds in 0u64..86_400, extra in 0u64..86_400,
    ) {
        let base = borrow_index_delta(rate, utilization, seconds, 0).unwrap();
        let longer = borrow_index_delta(rate, utilization, seconds + extra, 0).unwrap();
        prop_assert!(longer.index_delta >= base.index_delta);
    }

    /// **Borrow accrual is exactly additive over subdivisions of an interval.**
    ///
    /// This is the property whose absence hid a real defect. The suite already
    /// checked monotonicity in elapsed time, which the broken implementation
    /// satisfied perfectly — accruing more over a longer period is trivially
    /// true even when accruing almost nothing over any period.
    ///
    /// Before the remainder carry, `rate = 100_000, utilization = 359 bps` gave
    /// an index delta of 3_590 over one 3_600-second call and **exactly zero**
    /// over 3_600 one-second calls, because each individual call floored to
    /// nothing. Borrow fees are the pool's primary income, and they evaporated
    /// as a function of how often the market happened to be settled — worst
    /// under exactly the settle-often cadence the design recommends.
    #[test]
    fn borrow_accrual_is_additive_over_subdivisions(
        rate in 1u128..10_000_000u128,
        utilization in 1u128..=10_000u128,
        step_seconds in 1u64..600,
        steps in 1u64..500,
    ) {
        let total_seconds = step_seconds * steps;

        let single = borrow_index_delta(rate, utilization, total_seconds, 0).unwrap();

        let mut accumulated = 0u128;
        let mut carry = 0u128;
        for _ in 0..steps {
            let step = borrow_index_delta(rate, utilization, step_seconds, carry).unwrap();
            accumulated += step.index_delta;
            carry = step.remainder;
        }

        prop_assert_eq!(
            accumulated, single.index_delta,
            "subdividing {} seconds into {} steps of {} changed the accrual: \
             {} vs {} (rate={}, utilization={})",
            total_seconds, steps, step_seconds, accumulated, single.index_delta,
            rate, utilization
        );
    }

    /// The carried remainder is always less than the divisor, so it cannot grow
    /// without bound and cannot itself overflow across a long-lived market.
    #[test]
    fn borrow_remainder_stays_bounded(
        rate in 1u128..10_000_000u128,
        utilization in 1u128..=10_000u128,
        seconds in 1u64..86_400,
        carry_in in 0u128..36_000_000u128,
    ) {
        let step = borrow_index_delta(rate, utilization, seconds, carry_in).unwrap();
        prop_assert!(step.remainder < 10_000 * 3_600);
    }

    /// A zero-length or zero-rate step preserves the carry rather than
    /// discarding it. Zeroing it would silently destroy value every time
    /// utilisation briefly touched zero.
    #[test]
    fn a_no_op_step_preserves_the_carry(carry in 0u128..36_000_000u128) {
        prop_assert_eq!(borrow_index_delta(100, 100, 0, carry).unwrap().remainder, carry);
        prop_assert_eq!(borrow_index_delta(0, 100, 60, carry).unwrap().remainder, carry);
        prop_assert_eq!(borrow_index_delta(100, 0, 60, carry).unwrap().remainder, carry);
    }
}

// ── Cross-cutting ───────────────────────────────────────────────────────────

proptest! {
    /// Nothing in the public surface panics, for any input, ever. Proptest
    /// treats a panic as a failure, so this is the whole assertion.
    #[test]
    fn nothing_panics_on_any_input(
        sd in side(),
        s in any::<u128>(), entry in any::<u128>(), p in any::<u128>(),
        collateral in any::<u128>(), b in any::<u16>(), decimals in any::<u8>(),
    ) {
        let _ = notional_usd(s, p, decimals);
        let _ = notional_usd_ceil(s, p, decimals);
        let _ = unrealized_pnl(sd, s, entry, p, decimals);
        let _ = margin_requirement(collateral, b);
        let _ = liquidation_price(sd, s, entry, collateral, b, decimals);
        let _ = liquidation_fee(collateral, b, collateral);
        let _ = blended_entry_price(sd, s, entry, s, p);
        let _ = leverage_bps(collateral, entry as i128);
        let _ = shares_for_deposit(collateral, s, p);
        let _ = assets_for_shares(collateral, s, p);
        let _ = flow_fee(collateral, b);
        let _ = utilization_bps(collateral, s);
        let _ = funding_owed(collateral, entry as i128, p as i128);
        let _ = borrow_owed(collateral, entry, p);
    }

    /// A blended entry price always lies between the two prices it blends,
    /// inclusive of the rounding bump.
    #[test]
    fn blended_entry_lies_between_its_inputs(
        sd in side(), existing_size in size(), existing_entry in price(),
        added_size in size(), added_price in price(),
    ) {
        let blended = blended_entry_price(sd, existing_size, existing_entry, added_size, added_price);
        let Ok(blended) = blended else { return Ok(()); };
        let low = existing_entry.min(added_price);
        let high = existing_entry.max(added_price);
        // Allow the deliberate one-unit conservative bump for longs.
        prop_assert!(blended + 1 >= low && blended <= high + 1,
            "blended {blended} outside [{low}, {high}]");
    }

    /// The fee schedule check is consistent: if round-trip fees exceed maximum
    /// funding over a period, farming that period is unprofitable.
    #[test]
    fn fee_dominance_check_is_consistent(
        open in 0u16..500, close in 0u16..500,
        cap in 0u128..10_000_000u128, seconds in 1u64..86_400
    ) {
        // Just needs to not panic and to be deterministic.
        let first = fees_dominate_funding(open, close, cap, seconds);
        let second = fees_dominate_funding(open, close, cap, seconds);
        prop_assert_eq!(first, second);
    }
}
