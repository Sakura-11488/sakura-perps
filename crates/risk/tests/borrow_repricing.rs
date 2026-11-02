//! Counterexample to a rejected LP-pricing design, not an allowed instruction
//! sequence in the hardened program. LP minting requires no open claims, and
//! migrated books now integrate historical utilization before exposure changes.
//!
//! A fixed-claim valuation interval `[equity - reserve, equity + collateral]`
//! does not prove composed safety when a deposit changes the utilization used
//! to charge an earlier borrow interval. This legacy-model regression records
//! why share quotes alone could not establish safety; any future LP valuation
//! design still needs a separate compositional proof under the new accounting.

use sakura_perps_risk::funding::{borrow_index_delta, borrow_owed, fees_dominate_funding};
use sakura_perps_risk::scale::{BPS_DENOMINATOR, RATE_SCALE, SECONDS_PER_HOUR};

#[test]
fn conservative_lp_quotes_do_not_prevent_repricing_past_borrow() {
    // Six collateral decimals and unchanged price. A one-basis-point close fee
    // strictly dominates zero funding; a completely fee-free market is invalid.
    // The protocol receives 10% of retained fees. The borrow configuration fits
    // the program's legal 1%/hour rate and seven-day settle window.
    let one = 1_000_000u128;
    let equity = 1_000 * one;
    let shares = 1_000 * one;
    let collateral = 100 * one;
    let reserve = 100 * one;
    let notional = 1_000 * one;
    let deposit = 1_000 * one;
    let rate = RATE_SCALE / 100;
    let elapsed = 100 * SECONDS_PER_HOUR as u64;
    assert!(elapsed <= 7 * 24 * SECONDS_PER_HOUR as u64);
    assert!(fees_dominate_funding(0, 1, 0, SECONDS_PER_HOUR as u64).unwrap());
    let quoted_close_fee = notional / BPS_DENOMINATOR;
    assert_eq!(quoted_close_fee, 100_000);

    // The legacy accrual model fed terminal pool utilization to this function
    // for the whole elapsed interval. No deposit occurred in the baseline.
    let before_utilization = reserve * BPS_DENOMINATOR / equity;
    let baseline_index = borrow_index_delta(rate, before_utilization, elapsed, 0).unwrap();
    let baseline_borrow = borrow_owed(notional, baseline_index.index_delta, 0).unwrap();
    assert_eq!(before_utilization, 1_000);
    assert_eq!(baseline_borrow, 100_000_000);
    let baseline_gross = collateral - baseline_borrow;
    let baseline_fee = quoted_close_fee.min(baseline_gross);
    let baseline_payout = baseline_gross - baseline_fee;
    assert_eq!(baseline_payout, 0);
    assert_eq!(
        baseline_fee, 0,
        "no fee can be retained from an empty payout"
    );
    let baseline_lp_equity = equity + collateral - baseline_payout;

    // Hypothetical, NOT adopted: mint at the upper valuation bound. Quote
    // calculations here use independent integer arithmetic, not LP helpers.
    let upper_bound = equity + collateral;
    let minted = deposit * shares / upper_bound;
    assert_eq!(minted, 909_090_909);
    let funded_equity = equity + deposit;
    let diluted_utilization = reserve * BPS_DENOMINATOR / funded_equity;
    let changed_index = borrow_index_delta(rate, diluted_utilization, elapsed, 0).unwrap();
    let changed_borrow = borrow_owed(notional, changed_index.index_delta, 0).unwrap();
    assert_eq!(diluted_utilization, 500);
    assert_eq!(changed_borrow, 50_000_000);

    let changed_gross = collateral - changed_borrow;
    let changed_fee = quoted_close_fee.min(changed_gross);
    let protocol_fee = changed_fee / 10;
    let lp_fee = changed_fee - protocol_fee;
    let changed_payout = changed_gross - changed_fee;
    assert_eq!(changed_payout, 49_900_000);
    assert_eq!(protocol_fee, 10_000);
    assert_eq!(lp_fee, 90_000);
    let settled_lp_equity = funded_equity + collateral - changed_gross + lp_fee;
    let total_shares = shares + minted;
    assert_eq!(settled_lp_equity, 2_050_090_000);
    assert_eq!(total_shares, 1_909_090_909);

    // The position has closed, so reserve is zero and the proposed lower-bound
    // redemption is now exactly the ordinary settled share price. Waiting out
    // a withdrawal delay does not undo the earlier retroactive borrow change.
    let redeemed = minted * settled_lp_equity / total_shares;
    assert_eq!(redeemed, 976_233_333);
    let entry_exit_cost = deposit - redeemed;
    let borrow_saved = baseline_borrow - changed_borrow;
    let payout_increase = changed_payout - baseline_payout;
    assert_eq!(entry_exit_cost, 23_766_667);
    assert_eq!(borrow_saved, 50_000_000);
    assert_eq!(payout_increase, 49_900_000);

    // If the borrower and depositor are the same actor, the saved borrowing
    // exceeds the conservative entry premium. Existing LPs fund the difference.
    let combined_gain = payout_increase - entry_exit_cost;
    assert_eq!(combined_gain, 26_133_333);
    assert_eq!(
        baseline_lp_equity - (settled_lp_equity - redeemed),
        combined_gain + protocol_fee,
    );
}
