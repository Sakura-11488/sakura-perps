//! Checked fixed-point primitives.
//!
//! Every function here returns [`Result`]. Nothing panics, nothing wraps,
//! nothing saturates. Saturating arithmetic is specifically banned: it silently
//! corrupts accounting at the boundary instead of failing loudly, which is
//! strictly worse than a rejected transaction.
//!
//! The centrepiece is [`mul_div_floor`] / [`mul_div_ceil`], which compute
//! `a * b / c` with a full 256-bit intermediate. This matters more than it might
//! appear: `size × price` routinely reaches 10^32 against a `u128` ceiling of
//! 3.4 × 10^38, and LP share maths (`net_in × total_shares`) can reach 10^30.
//! Doing the multiply first and the divide second is required for precision, but
//! only safe with a wide intermediate.
//!
//! Rust has no native `u256` and pulling a bignum crate into a program that will
//! hold money is more supply-chain risk than sixty lines of arithmetic.

use crate::error::RiskError;
use crate::scale::MAX_POW10;

/// `10^exp`, or [`RiskError::MathOverflow`] if it would not fit in `u128`.
///
/// Deliberately not `10u128.pow(exp)`, which **panics** for `exp > 38`. Oracle
/// exponents arrive from off-chain data and are not to be trusted to be sane.
pub fn pow10(exp: u32) -> Result<u128, RiskError> {
    if exp > MAX_POW10 {
        return Err(RiskError::MathOverflow);
    }
    Ok(10u128.pow(exp))
}

/// Full 128 × 128 → 256 bit product, returned as `(high, low)`.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const LOW_MASK: u128 = u64::MAX as u128;

    let (a_lo, a_hi) = (a & LOW_MASK, a >> 64);
    let (b_lo, b_hi) = (b & LOW_MASK, b >> 64);

    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;

    // Sum the cross terms with the carry out of the low product. Each addend is
    // at most 2^64 - 1 squared, so this cannot overflow u128.
    let mid = (ll >> 64) + (lh & LOW_MASK) + (hl & LOW_MASK);

    let low = (ll & LOW_MASK) | (mid << 64);
    let high = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);

    (high, low)
}

/// Compare `a × b` against `c × d` exactly — no division, no rounding.
///
/// Exists so a ratio comparison can be written as the true rational instead of
/// as a comparison between two floored quotients. `floor(x) <= floor(y)` is not
/// `x <= y`, and that substitution is exactly what let a utilisation of
/// 100.005% satisfy a 100% ceiling: both floored to 10 000 bps.
///
/// Both products go through the full 256-bit intermediate, so no combination of
/// `u128` inputs can overflow and there is no fast path to get wrong.
pub fn cmp_products(a: u128, b: u128, c: u128, d: u128) -> core::cmp::Ordering {
    mul_wide(a, b).cmp(&mul_wide(c, d))
}

/// Divide a 256-bit value `(high, low)` by `d`, returning `(quotient, remainder)`.
///
/// Errors with [`RiskError::MathOverflow`] when the quotient would not fit in
/// `u128`, which is exactly the case `high >= d`.
fn div_wide(high: u128, low: u128, d: u128) -> Result<(u128, u128), RiskError> {
    if d == 0 {
        return Err(RiskError::DivisionByZero);
    }
    if high >= d {
        // The quotient would be at least 2^128.
        return Err(RiskError::MathOverflow);
    }

    // Shift-subtract long division. `remainder` starts as the high word, which
    // the check above guarantees is already less than `d`.
    let mut remainder = high;
    let mut quotient: u128 = 0;

    for i in (0..128).rev() {
        // If the top bit is set, `remainder << 1` loses it — but that lost bit
        // means the true value is at least 2^128, hence certainly >= d, so the
        // subtraction is required and `wrapping_sub` yields the correct low
        // 128 bits. This is the standard trick and the only subtle line here.
        let overflowed = remainder >> 127 == 1;
        remainder = (remainder << 1) | ((low >> i) & 1);

        if overflowed || remainder >= d {
            remainder = remainder.wrapping_sub(d);
            quotient |= 1u128 << i;
        }
    }

    Ok((quotient, remainder))
}

/// `floor(a * b / c)` with a full 256-bit intermediate.
///
/// Rounds **down**. Use where rounding down favours the pool: shares minted,
/// assets returned, collateral credited, trader profit.
pub fn mul_div_floor(a: u128, b: u128, c: u128) -> Result<u128, RiskError> {
    if c == 0 {
        return Err(RiskError::DivisionByZero);
    }
    // Fast path: the product fits, so no wide arithmetic is needed.
    if let Some(product) = a.checked_mul(b) {
        return Ok(product / c);
    }
    let (high, low) = mul_wide(a, b);
    div_wide(high, low, c).map(|(quotient, _)| quotient)
}

/// `ceil(a * b / c)` with a full 256-bit intermediate.
///
/// Rounds **up**. Use where rounding up favours the pool: fees charged, margin
/// required, trader losses.
pub fn mul_div_ceil(a: u128, b: u128, c: u128) -> Result<u128, RiskError> {
    if c == 0 {
        return Err(RiskError::DivisionByZero);
    }
    if let Some(product) = a.checked_mul(b) {
        // `product / c` then bump if there was any remainder. Written this way
        // rather than `(product + c - 1) / c`, which overflows near u128::MAX.
        let quotient = product / c;
        return if product % c == 0 {
            Ok(quotient)
        } else {
            quotient.checked_add(1).ok_or(RiskError::MathOverflow)
        };
    }
    let (high, low) = mul_wide(a, b);
    let (quotient, remainder) = div_wide(high, low, c)?;
    if remainder == 0 {
        Ok(quotient)
    } else {
        quotient.checked_add(1).ok_or(RiskError::MathOverflow)
    }
}

/// `ceil(a / b)` without an intermediate that can overflow.
pub fn ceil_div(a: u128, b: u128) -> Result<u128, RiskError> {
    if b == 0 {
        return Err(RiskError::DivisionByZero);
    }
    let quotient = a / b;
    if a % b == 0 {
        Ok(quotient)
    } else {
        quotient.checked_add(1).ok_or(RiskError::MathOverflow)
    }
}

/// Signed `a * b / c`, truncating toward zero, with a 256-bit intermediate.
///
/// Used for cumulative funding, which is genuinely signed. Truncation toward
/// zero is deliberate: it shrinks the magnitude of whatever is owed in both
/// directions, so neither side can farm the rounding by choosing a sign.
pub fn mul_div_i128(a: i128, b: i128, c: i128) -> Result<i128, RiskError> {
    if c == 0 {
        return Err(RiskError::DivisionByZero);
    }
    let negative = (a < 0) ^ (b < 0) ^ (c < 0);

    let magnitude = mul_div_floor(unsigned_abs(a), unsigned_abs(b), unsigned_abs(c))?;

    if negative {
        if magnitude > (i128::MAX as u128) + 1 {
            return Err(RiskError::MathOverflow);
        }
        // -(i128::MAX + 1) is i128::MIN and is representable, but negating it
        // through i128 is not, so handle it explicitly.
        if magnitude == (i128::MAX as u128) + 1 {
            Ok(i128::MIN)
        } else {
            Ok(-(magnitude as i128))
        }
    } else {
        if magnitude > i128::MAX as u128 {
            return Err(RiskError::MathOverflow);
        }
        Ok(magnitude as i128)
    }
}

/// `|value|` as a `u128`, correct for [`i128::MIN`], which has no positive
/// counterpart in `i128`.
fn unsigned_abs(value: i128) -> u128 {
    if value < 0 {
        // Cast first, then negate in the unsigned domain, so i128::MIN works.
        (value as u128).wrapping_neg()
    } else {
        value as u128
    }
}

/// Checked `u128` → `u64`, for converting a computed USD or token value back
/// into something an SPL token account can hold.
pub fn to_u64(value: u128) -> Result<u64, RiskError> {
    u64::try_from(value).map_err(|_| RiskError::MathOverflow)
}

/// Checked `i128` → `u64`, rejecting negatives.
pub fn to_u64_signed(value: i128) -> Result<u64, RiskError> {
    if value < 0 {
        return Err(RiskError::NegativeAmount);
    }
    to_u64(value as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow10_covers_the_full_range_and_rejects_beyond() {
        assert_eq!(pow10(0).unwrap(), 1);
        assert_eq!(pow10(6).unwrap(), 1_000_000);
        assert_eq!(pow10(38).unwrap(), 10u128.pow(38));
        // The case that would panic with 10u128.pow.
        assert_eq!(pow10(39), Err(RiskError::MathOverflow));
        assert_eq!(pow10(u32::MAX), Err(RiskError::MathOverflow));
    }

    #[test]
    fn mul_div_uses_the_wide_path_when_the_product_overflows() {
        // u128::MAX * 2 / 4 == u128::MAX / 2, unrepresentable without a wide
        // intermediate.
        let result = mul_div_floor(u128::MAX, 2, 4).unwrap();
        assert_eq!(result, u128::MAX / 2);
    }

    #[test]
    fn mul_div_floor_and_ceil_bracket_the_true_value() {
        let (a, b, c) = (u128::MAX, 3, 7);
        let floor = mul_div_floor(a, b, c).unwrap();
        let ceil = mul_div_ceil(a, b, c).unwrap();
        assert_eq!(
            ceil - floor,
            1,
            "inexact division should differ by exactly 1"
        );
    }

    #[test]
    fn mul_div_ceil_does_not_bump_on_exact_division() {
        assert_eq!(mul_div_ceil(10, 10, 5).unwrap(), 20);
        assert_eq!(mul_div_floor(10, 10, 5).unwrap(), 20);
    }

    #[test]
    fn quotient_that_cannot_fit_is_an_error_not_a_wrap() {
        // u128::MAX * u128::MAX / 1 is ~2^256 and cannot be represented.
        assert_eq!(
            mul_div_floor(u128::MAX, u128::MAX, 1),
            Err(RiskError::MathOverflow)
        );
    }

    #[test]
    fn division_by_zero_is_rejected_everywhere() {
        assert_eq!(mul_div_floor(1, 1, 0), Err(RiskError::DivisionByZero));
        assert_eq!(mul_div_ceil(1, 1, 0), Err(RiskError::DivisionByZero));
        assert_eq!(ceil_div(1, 0), Err(RiskError::DivisionByZero));
        assert_eq!(mul_div_i128(1, 1, 0), Err(RiskError::DivisionByZero));
    }

    #[test]
    fn signed_mul_div_truncates_toward_zero_in_both_directions() {
        assert_eq!(mul_div_i128(7, 1, 2).unwrap(), 3);
        assert_eq!(mul_div_i128(-7, 1, 2).unwrap(), -3);
        assert_eq!(mul_div_i128(7, -1, 2).unwrap(), -3);
        assert_eq!(mul_div_i128(-7, -1, 2).unwrap(), 3);
    }

    #[test]
    fn i128_min_does_not_break_absolute_value() {
        // The classic overflow: -i128::MIN is not representable.
        assert_eq!(unsigned_abs(i128::MIN), (i128::MAX as u128) + 1);
        assert_eq!(mul_div_i128(i128::MIN, 1, 1).unwrap(), i128::MIN);
        assert_eq!(mul_div_i128(i128::MIN, 1, 2).unwrap(), i128::MIN / 2);
    }

    #[test]
    fn conversions_reject_out_of_range_values() {
        assert_eq!(to_u64(u64::MAX as u128).unwrap(), u64::MAX);
        assert_eq!(to_u64(u64::MAX as u128 + 1), Err(RiskError::MathOverflow));
        assert_eq!(to_u64_signed(-1), Err(RiskError::NegativeAmount));
    }
}
