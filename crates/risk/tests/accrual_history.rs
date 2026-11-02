//! Regression proofs for cumulative historical borrowing and signed funding.

use sakura_perps_risk::funding::{
    borrow_index_delta_from_integral, funding_index_delta_with_carry,
};

#[test]
fn funding_carry_preserves_tiny_positive_and_negative_rates() {
    for rate in [1, -1, 7, -7, 3_601, -3_601] {
        let once = funding_index_delta_with_carry(rate, 3_600, 0).unwrap();
        let (mut index, mut carry) = (0, 0);
        for _ in 0..3_600 {
            let step = funding_index_delta_with_carry(rate, 1, carry).unwrap();
            index += step.index_delta;
            carry = step.remainder;
        }
        assert_eq!((index, carry), (once.index_delta, once.remainder));
        assert_eq!(index, rate);
    }
}

#[test]
fn signed_funding_is_partition_invariant_through_rate_changes() {
    let history = [(3_601, 1u64), (-2, 1), (-7, 719), (11, 5_400), (-1, 83)];
    let (mut coarse_index, mut coarse_carry) = (0, 0);
    let (mut fine_index, mut fine_carry) = (0, 0);
    let mut exact_numerator = 0i128;
    for (rate, seconds) in history {
        let step = funding_index_delta_with_carry(rate, seconds, coarse_carry).unwrap();
        coarse_index += step.index_delta;
        coarse_carry = step.remainder;
        exact_numerator += rate * i128::from(seconds);
        for _ in 0..seconds {
            let step = funding_index_delta_with_carry(rate, 1, fine_carry).unwrap();
            fine_index += step.index_delta;
            fine_carry = step.remainder;
        }
        assert_eq!((coarse_index, coarse_carry), (fine_index, fine_carry));
        assert_eq!(coarse_index, exact_numerator.div_euclid(3_600));
        assert_eq!(i128::from(coarse_carry), exact_numerator.rem_euclid(3_600));
    }
}

#[test]
fn accrual_helpers_fail_closed_on_overflow_and_invalid_funding_carry() {
    assert!(funding_index_delta_with_carry(i128::MAX, 2, 0).is_err());
    assert!(funding_index_delta_with_carry(i128::MIN, 2, 0).is_err());
    assert!(funding_index_delta_with_carry(1, 1, -1).is_err());
    assert!(funding_index_delta_with_carry(1, 1, 3_600).is_err());
    assert!(borrow_index_delta_from_integral(u128::MAX, 2, 0).is_err());
}

#[test]
fn borrow_carry_survives_rate_changes_and_zero_utilization() {
    let first = borrow_index_delta_from_integral(7, 1_003, 0).unwrap();
    let idle = borrow_index_delta_from_integral(0, 50_000, first.remainder).unwrap();
    let last = borrow_index_delta_from_integral(13, 9_001_001, idle.remainder).unwrap();
    let numerator = 7 * 1_003 + 13 * 9_001_001;
    assert_eq!(first.index_delta + last.index_delta, numerator / 36_000_000);
    assert_eq!(last.remainder, numerator % 36_000_000);
}
