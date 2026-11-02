# Sakura economic review notes

Contemporaneous review, corrected against fresh SBF tests to include legally required fees. See the [integrated report](../2026-11-02-percolator-and-risk-rooms.md) for final validation status.

Sakura Perps security and community design review
Reviewed baseline: /Users/freemellr/code/sakura-perps, 2bb0f71b12700e64dce172a9ddf4abc1ae51d258
Working change branch: codex/percolator-risk-rooms
Date: 2026-11-02

Scope and status
- Read the supplied Sakura AGENTS instructions, repo docs/research/2026-10-30-perps-direction.md, relevant M5 specification sections, security-hardening guide, risk pool/position/funding and program pool/position/market/oracle code.
- No AGENTS.md exists in sakura-perps or its parent chain. Sakura web hosting instructions do not authorize a perps deployment. No deployment or push performed.
- Preserved the pre-existing README modification and untracked docs/research directory.
- Economic traces below are derived from executable formulas with a legal1bps close fee and10% protocol fee share. All99 SVM integration tests, including the new runtime regressions, passed against fresh SBF artifact SHA2561d0656ffe2f336fd87e2f82ff1918456d1712a45a867672ee4f4ac5104198a6b.

1. Highest-priority immediate correction: forbid pricing LP shares with open claims

Baseline pool.rs handle_lp_deposit prices minting against quote_deposited. handle_lp_withdraw prices redemption against the same value. Neither marks trader equity. Reserve checks ensure payout capacity, but do not allocate gains/losses fairly between entering, exiting and remaining LPs. A withdrawal delay only delays collection of the transfer.

Exact legal winner/exit trace (quote-token units; open fee0, close fee1bps, protocol share10%; spread/funding/borrow zero):
1. Seed A=1,000, S=1,000 shares including the one permanently locked minimum share.
2. Open long notional 1,000 at price100, collateral C=200, max_profit_bps1,000 => R=100.
3. Price rises to110; gross trader payout is300, close fee0.11, net payout299.89. LPs retain0.099 of the fee, so eventual LP equity is900.099 while tracked A remains1,000.
4. An LP requests and, after the configured delay, redeems400 shares. Baseline pays400; fair settled entitlement is360.0396.
5. Remaining A600 and R100 satisfy 100/600 <=20%. Closing trader then leaves A500.099/S600.
6. The early LP extracted39.9604 from remaining LPs despite every original vault/reserve invariant holding.

Exact legal loser/entry trace:
1. Same initial state, but price falls to90; trader gross payout is100, net payout99.91, and LP close-fee revenue0.081. Eventual LP equity is1,100.081.
2. New LP deposits1,000 before the trader closes; baseline mints1,000 shares, half of total2,000.
3. Trader closes, pool A becomes2,100.081.
4. After the withdrawal delay the entrant redeems for1,050.0405, taking50.0405 of the existing LPs' previously earned but unmarked value.

Containment implemented:
- New helper assert_lp_valuation_available requires locked_quote==0 AND reserved_quote==0.
- Both LP mint and redemption handlers call it before financial CPIs/mutations.
- Uses new parent-owned PerpsError::LpValuationUnavailable.
- Requests/cancellations, voluntary close, admin settlement, keeper liquidation and emergency close are not gated by this helper.
- Both counters are checked independently; a reserve-only check misses unrecognized collateral losses, and a collateral-only check is not enough to cover all future/legacy reserve states.
- No account layout or instruction account-list change in this LP correction.

Limits to describe honestly:
- This is a temporary lockup policy, not liability-aware NAV and not a mainnet security certification.
- An outstanding position can block all pool LP entries/redemptions indefinitely. Since markets share a pool, a position in one market blocks LP flows for every market. New opens can delay the empty-book window. An operational wind-down must stop new risk and settle existing positions; the patch must not promise a bounded exit time.
- Deposits can no longer recapitalize an active book when the utilization ceiling has been lowered. Existing closes still reduce utilization under the monotone invariant. Keep those close paths and cancellation available.
- The reserve ceiling bounds positive profit commitments relative to tracked equity. It does not bound all valuation errors: omitted trader losses can understate LP equity without being bounded by reserved_quote.
- Do not subtract the full reserve from NAV and call the result fair value; reserves bound possible profit, rather than measuring actual profit.

2. Alternative conservative bid/ask LP pricing: valid bounds, unsafe composition here

Proposed bounds: lower L=A-R and upper U=A+C, with A tracked LP equity, R total reserved profit and C total locked trader collateral. At a fixed settlement snapshot these bounds are correct even with funding, borrowing, close fees, liquidation fees and keeper compensation.

Proof for each position: the ledger changes LP equity by C_i - G_i + F_i, where gross payout 0<=G_i<=C_i+R_i and retained LP fees 0<=F_i<=G_i. Therefore -R_i <= delta_i <= C_i, and A-R <= eventual LP equity <= A+C. Bad debt is informational, not an extra collectible liability. Funding credits and borrowing charges are already inside G_i and cannot bypass its cap.

At FIXED claims, mint M=floor(D*S/U) and redeem Q=floor(q*L/S) are conservative:
- M<=D*S/E implies existing LPs are not diluted by entry at the same E.
- Q<=q*E/S implies a departing LP is not overpaid at the same E.
- R/A utilization checks must continue to use tracked accounting, independently of price bounds; reserve insolvency must fail closed rather than saturating a negative L to zero.

But claims are NOT fixed when LP flows change A: market::accrue uses CURRENT global R/A for the entire previously unaccrued interval. LP flows do not accrue every market first. This invalidates the composed safety claim.

Reachable counterexample even under the conservative upper/lower quotes:
- Legal configuration: open fee0, close fee1bps, protocol fee share10%, spread/oracle-drift/funding0, initial margin10%, max_profit10%, borrow_rate_per_hour1% (allowed maximum), max_settle_window_seconds100 hours (below allowed seven days). A zero round-trip fee is invalid because the funding-dominance inequality is strict; the executable arithmetic regression now asserts that the positive fee is legal.
- A=1,000, S=1,000; trader opens N=1,000, C=100, R=100. Price remains100. No market accrual for100 hours. At original10% utilization the trader owes100 borrowing; settling now pays trader0 and gives old LPs1,100.
- Same trader is also a newcomer LP. Before settling, deposit D=1,000 at U=1,100; minted shares909.090909 (floor at six decimals).
- A is now2,000; market::accrue uses5% utilization retrospectively for all100 hours and charges only50 borrowing. Gross payout50 retains a0.1 close fee; net trader payout is49.9, LP fee revenue0.09 and protocol revenue0.01.
- Pool A becomes2,050.09, S becomes1,909.090909. After claims are closed and withdrawal delay passes, new LP redeems exactly976.233333 token units at six decimals.
- Combined actor saves50 borrowing, pays0.1 close fee and23.766667 LP premium, net gain26.133333. Old LPs lose26.143333, which equals actor gain plus0.01 protocol fees. No price move is needed.
- This requires a permitted no-crank interval; a liquidation keeper might stop it sooner but a keeper availability assumption cannot establish invariant security. Baseline default test parameters use borrow0, so default fixtures would miss the issue.

Recommendation: do not ship conservative quote pricing as the financial fix. First resolve historical utilization accrual (e.g., a pool-global cumulative utilization integral checkpointed on every relevant mutation, with per-market rate changes also checkpointed), prove composed accounting, and decide the explicit premium/haircut policy. Just accruing the touched market is insufficient because borrowing is coupled across all markets.

Even after accrual is corrected, conservative quotes are a product change:
- Incumbents can amplify entry premiums by opening very overcollateralized positions; collateral has no upper cap. An opener can increase exit haircuts by reserving profit capacity.
- Meaningful user slippage bounds prevent quote changes after signing, but do not make a displayed punitive quote fair. LP calls currently accept min_shares_out/min_amount_out0, so clients must not imply defaults protect the user.
- Conservative prices favor remaining LPs and do not satisfy general settlement-order fairness. Waiting until unused reserve releases changes entitlement. Identical-direction flows at unchanged bounds are proportional before rounding, but mixes of entries, exits and settlement transfer different amounts.
- At the utilization ceiling, any withdrawal can still fail even with a lower NAV quote. This preserves some liveness, not guaranteed exits.

3. Emergency oracle findings (policy work, not changed here)

- last_good_price is written by ordinary open/close, permissionless refresh, admin liquidation and keeper liquidation.
- Liquidation paths intentionally accept looser confidence/age limits, but overwrite the same reference later used by emergency_close_position.
- last_good_price_ts records current Clock time rather than the oracle publish_time; repeated acceptance can describe an older observation as newly observed. There is no monotonic publication-time gate on the stored reference.
- Refresh and exits clamp to an EMA band; invalid EMA is mapped to0 and clamps pass the spot through. Therefore 'last_good' does not mean a strict two-source or nondivergent settlement mark.
- Emergency close requires quarantine and a delay, then uses remembered price with confidence0 or falls back to each position's own entry when no remembered price exists. No age/provenance rule prevents extremely stale, differently sourced or per-position fallback settlement.
- Separating strict settlement observations from liquidation observations is sensible, but a freshness gate alone can permanently trap the only oracle-free exit. Define a full frozen settlement snapshot, authority/quorum/challenge/fallback and degraded-state policy before changing these paths.
- Any new provenance flag must handle already-persisted pre-upgrade prices; treating an old last_good_price as if it satisfied a new strict rule would be unsound.

4. Proposed community-season design: credible direction with explicit tradeoffs

Individually owned maximum-loss budgets
- Define a token-denominated cap on external collateral contributions into an isolated account, enforceable by the program. A stop-loss target cannot promise execution through gaps.
- Specify gross cumulative contributions vs current wallet debit, realized gains reinvestment, top-ups, gas, sponsored fees and stablecoin value changes. A member can lose a reinvested profit balance larger than their original contribution; receipts must say which cap is meant.
- Captains/social roles get no trading or withdrawal authority. Any session key gets a bounded market/notional/price/fee/expiry scope with explicit revocation and no right to expand limits.
- The program upgrade authority can change enforcement; receipts must expose upgrade powers and delay/exit rights rather than promise immutable budgets.

Isolated community season capital
- Use separate escrow and ledger per capital vintage; trader accounts, LP underwriting capital and sponsor prize treasury need separate permissions and liabilities. Another team's winnings or sponsor money must not quietly become a backstop.
- Fully reserve each maximum promised payout and account for fee/keeper outflows. Correlated positions and Sybil wallets cannot be treated as diversified when reserving capacity.
- A bounded payout curve is a different payoff than unlimited conventional perp profit. A finite season is a scoring window unless the financial contract explicitly expires or has a stated transfer/wind-down mechanism.
- Closed-duration underwriting can make liability enumeration easier but fragments liquidity and locks capital. Perpetual claims cannot honestly promise finite LP redemption without a specified settlement or transfer rule.

Pro-rata or epoch settlement
- 'No first exit advantage' requires freezing the same cohort, valuation marks, funding cutoff, complete liabilities, total distributable capital and claim denominators before anyone redeems.
- Compute each claim from the original frozen numerator/denominator, record claimed status, and define rounding dust independently of claim order. Dividing the currently remaining pool by currently presented claims recreates a bank run.
- Late discovery of liabilities, new positions, new deposits or new fee debits must not alter a supposedly final cohort. Large books need bounded-progress enumeration with completeness commitments and recovery if a keeper stops.
- Fully funded maximum claims should be payable without haircut. If the proposal instead promises pro-rata haircuts when funds fall short, claims are contingent; the UI/terms must not also call the profit cap guaranteed. Explicitly define whether principal, profits, fees and insurance are pari passu or senior/junior.
- Epochs trade immediate withdrawal for common pricing, may concentrate MEV at boundaries and can stall during oracle failure. Freeze prices deterministically with provenance; batch processing alone does not solve price manipulation or discretionary timing.

Community UX and novelty
- Cooperative strategy commitments, analyst/risk-reviewer roles, explainable scorecards and optional delayed disclosure are a useful integration hypothesis, not evidence of a new financial primitive.
- Prefer a paper-only pilot with equal initial budgets and deterministic fee/funding/latency assumptions. Team membership must be locked before scoring so losing wallets cannot be dropped. Wallet count is not person count.
- Reward turnover/leverage creates incentives to increase risk. A multi-metric post-season review is more honest than declaring the largest short-term return 'skill'. Sponsor rewards, if introduced, should be independently bounded.

Primary prior-art references verified during this review (not an exhaustive novelty search):
- GMX Synthetics implementation README: reserves, pending PnL and profit limits already exist. https://github.com/gmx-io/gmx-synthetics/blob/main/README.md
- ERC-7540, created2023-10-18: asynchronous request/claim deposit and redemption flows are established; the standard does not by itself prove fair valuation or loss allocation. https://eips.ethereum.org/EIPS/eip-7540
- Hyperliquid HIP-4 docs: fully collateralized bounded outcome contracts and dated settlement already exist; they are not ordinary leveraged perpetuals. https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-4-outcome-markets
- Existing repo research additionally catalogs team competitions, equal-budget research contests, paper seasons, prop challenge loss limits, perpetual options, and external incident reports. Those are useful directions for deeper comparison, not deployment/audit assurances.

5. Changes and tests in this agent's owned files

programs/sakura-perps/src/pool.rs:
- Fail-closed LP valuation helper and both flow checks.
- Update core documentation to stop claiming withdrawal delay alone solves adverse selection.
- Host regression: both counters zero accepted; collateral-only/reserve-only/both-nonzero refused.

crates/svm-tests/tests/positions.rs:
- Configurable seeded liquidity and LP request/withdraw/cancel/deposit instruction helpers.
- Winner-before-withdraw regression: otherwise-valid400 redemption fails unchanged; trader closes for299.89 net; same matured request then redeems360.0396; fresh flat-book deposit works.
- Loser-before-entry regression: newcomer deposit fails unchanged; trader closes for99.91 net; newcomer then buys909.023971 shares for1,000 at settled1,100.081 equity.
- Open-book withdrawal request/cancellation remains available under all pause flags.
- Intent helper fields for the parallel agent's market revision/open expiry/position generation patch.
- Stale open after no-op retune refuses without token/index/counter changes; fresh consent succeeds.
- Zero and expired opening deadlines refuse, exact deadline slot succeeds, rejected opens consume no ID.
- Old close fails against a new position at the same PDA in the same slot; fresh generation close succeeds.
- Market risk revision and position sequence u64 overflow both roll back.
- Legacy position generation0 remains closeable without account resizing.

No source/API claim here should be read as release-ready while emergency policy, historical accrual, economic fuzzing, operational proof and independent review remain open.

Verification update:
- `cargo test -p sakura-perps-svm-tests --locked --no-run` passed, compiling all integration executables with the new LP and intent regressions.
- `cargo test -p sakura-perps-risk --locked --test borrow_repricing`: 1 passed.
- `cargo test -p sakura-perps --lib --locked lp_valuation_requires_both_claim_counters_to_be_zero`: 1 passed, 26 filtered out.
- Added crates/risk/tests/borrow_repricing.rs: explicit independent integer quote model for the rejected conservative pricing approach. Expected combined actor extraction is exactly26,133,333 quote atoms (six decimals), including the legal1bps close fee and10% protocol fee share.
- Formatting and git diff --check pass. Fresh SBF runtime execution passed all99 tests:1 layout,8 admin rotation,3 collateral guard,12 oracle,57 position,18 vault. No failed/ignored tests. Full log: /tmp/sakura-perps-toolchain/svm-tests.log.

Final verification after correcting the fixture's invalid zero-fee setup:
- All3 focused LP runtime tests pass under openFee0/closeFee1bps/protocolShare10%.
- Corrected arithmetic regression passes1/1 and explicitly asserts legal fee dominance.
- Full SVM suite passes99/99 against the unchanged fresh artifact hash above.
- Original zero-fee traces were an invalid program configuration; the corrected positive-fee traces above replace them completely. No production validation was weakened.
