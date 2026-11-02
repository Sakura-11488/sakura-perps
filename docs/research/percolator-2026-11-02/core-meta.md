# Core and Meta review notes

Contemporaneous source-review notes. See the [integrated report](../2026-11-02-percolator-and-risk-rooms.md) for final implementation and validation status.

Percolator core + Meta: pinned primary-source review for Sakura Perps
Reviewed 2026-11-02; read-only upstream inspection. No upstream build scripts, tests, binaries or setup commands executed. No Sakura source edited by this researcher.

PROVENANCE
- Fresh shallow clone: /tmp/sakura-percolator-research/percolator
  branch master, HEAD 37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311
  commit date 2026-10-28T17:50:48-06:00; Merge pull request #244 from aeyakovenko/fix/restore-233-fuzz
  Apache-2.0: Cargo.toml:5 and LICENSE.
- Fresh shallow clone: /tmp/sakura-percolator-research/percolator-meta
  branch master, HEAD d1a147cc36d3302fc2115d77a8adae7b4fa87241
  commit date 2026-07-18T16:53:16-06:00; Merge pull request #258 from aeyakovenko/agent/real-dos-stale-own-vault-exit
  Apache-2.0: LICENSE:2-4.
- All percolator links below are pinned to the first SHA, Meta links to the second. These are not the same engine/wrapper combination Meta pins internally.

WHAT CURRENT CORE ACTUALLY IS
Current core is v16.9.1: one market-group quote vault, many asset slots and bounded account-local portfolio views. It is a zero-copy risk library, not a deployed exchange, router, authenticated oracle adapter, or token-custody program. Account loading/authentication, oracle normalization, token movement, CU limits and transaction rollback are external assumptions. No reason to replace Sakura's native Anchor pool with this library to obtain its useful principles.
Evidence: README.md:9-26,48-54,135-139.
https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/README.md#L9-L54
The repository labels itself educational, not production ready, and unaudited (README:3-4). Do not call it production-proven or claim these proofs certify an integration.

ACCOUNTING AND SOLVENCY
1. Positive PnL has source provenance (asset, opposing side); not all positive numbers are cash. Capital and recoverable backing are senior stocks; derivative positive PnL is a junior claim. A profitable A leg can support a B leg within the same instance only up to realizable A-source backing. Different instances do not share health, insurance, payouts or solvency.
   spec.md:4,28,36-45,1104-1177; src/v16.rs:9230-9249.
   residual() subtracts c_tot + insurance + backing-provider earnings + fresh recoverable backing principal from vault. This prevents paying a winner from the same atoms a provider may later withdraw.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/src/v16.rs#L9230-L9249
2. Credit rate is min(available backing / positive claim bound, 1), computed with U256. Available excludes already liened counterparty backing and valid/impaired insurance encumbrances. Negative PnL stays fully negative; credit is conservatively discounted.
   src/v16.rs:1223-1255,12859-12878.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/src/v16.rs#L1223-L1255
3. Durable use of positive PnL requires a reserved/consumed lien or a no-positive-credit approval path. A mere aggregate backing count is insufficient: an actual fresh bucket or insurance reservation must fund the lien. Credit reservations are encumbrances over stocks, NOT new token value. Separate value-flow and reservation witnesses prevent counting the same atom as capital and collateral twice. Fees that cannot be collected are forgiven rather than charged to insurance/social loss.
   spec.md:44-49,61,1244-1282; src/v16.rs:19608-19739 (consume source credit, burn face, credit capital, balanced flow).
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/spec.md#L1256-L1282
4. Maturity clarification: spec.md:1211 includes maturity_or_warmup_factor in a generic leg-credit formula. Current source does NOT expose the old simple time-based warmup model as its main safety mechanism. Source-wide search found pnl_matured_pos_tot initialization, shape check and reductions, but no production clock-based increasing maturity transition. Live conversion instead checks released PnL, favorable-action eligibility, absence of source liens/active source exposure, and realizable backing. Do not describe timed maturity as an implemented universal guarantee based on old Percolator descriptions or this spec placeholder.
   src/v16.rs:19560-19605,19721-19725. https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/src/v16.rs#L19560-L19605

LOSS WATERFALL AND WITHDRAWALS
5. Bankruptcy support includes senior capital, realized nonjunior gains and consumed settlement-quality liens. Soft/open positive PnL cannot cure a bankruptcy residual. Next comes allocated domain insurance, then residual bookkeeping to the ORIGINAL asset/opposing-side loss domain, with quantity ADL only after the residual is durable. Insurance-backed source-credit support is counted once as support, not again as direct insurance spending. Pending loss obligations follow exiting accounts, preventing first-exit avoidance.
   spec.md:1514-1565; src/v16.rs:1840-1858 implements cap-by-deficit and cap-by-domain insurance arithmetic.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/spec.md#L1514-L1565
6. Residual B-index accrual carries remainder rather than erasing sub-atom loss. Loss domains are isolated; no global B index socializes an unrelated asset's failure. Close serialization admits at most one active close per domain and per account; immutable maximum close lifetime eventually routes stalled work into recovery. Token conservation alone does not prove those routes remain reachable under real CU/account restrictions.
   spec.md:1360-1384,1450-1472,1488-1511.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/spec.md#L1450-L1472
7. Live withdraw_not_atomic is deliberately narrower than the generic withdraw-equity formula: require Live, no active bitmap, no active close ledger; settle negative PnL from principal; amount <= capital; no negative equity after; decrease capital aggregate and vault together. Positive PnL must first pass separate conversion. Every failed call relies on the caller's atomic rollback.
   src/v16.rs:20117-20184.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/src/v16.rs#L20117-L20184
8. Insurance withdrawal is domain-reservation-aware, and provider withdrawal must preserve amounts still needed by unresolved/resolved winners. Resolved payout rate is min(snapshot residual * scale, exact receipts + unreceipted bound) / total bound; receipts can claim top-ups when bounds refine or backing expires.
   src/v16.rs:11662-11697,1978-2019,20212-20239; spec.md:1167.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/src/v16.rs#L1978-L2019

ORACLE AND PORTFOLIO LIMITS
9. Core receives oracle target integers; it does not authenticate Pyth signatures. Target setters reject zero/out-of-range target, duplicate assets, non-Live mode, and advance an oracle epoch. The portfolio health certificate is tied to oracle/funding/risk/asset-set epochs. Health uses conservative source-backed equity, gross requirements and an adverse target-versus-effective-price lag penalty.
   src/v16.rs:15267-15325,14569-14642.
   https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/src/v16.rs#L14569-L14642
10. Cross-margin does not imply statistical portfolio margin or automatic correlated hedging. Current public profile has zero hedge credit and numeric concentration/thin-market penalties; stale/impaired/locked situations use hard gates or progress-only routes. Future hedge credit needs exact worst-case combined price/funding/basis/oracle envelope proofs. Routing/best execution are wrapper/matcher responsibilities; Meta adds governance/rewards, not a router.
    spec.md:1288-1348,1659; README.md:135-139.
    https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/spec.md#L1288-L1348
11. Recovery fallback price knobs are explicitly reserved, not implemented. v16.9 uses last authenticated effective price and terminal recovery/resolution, not synthetic oracle substitutes. Similarly drift_consumed is reserved zero; close lifetime replaces an earlier never-implemented drift reserve. Do not turn named fields into feature claims.
    spec.md:9,56-66,1501-1511.
    https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/spec.md#L56-L66

FORMAL VERIFICATION: EVIDENCE AND LIMITS
12. This is bounded, decomposed verification of production kernels plus harness assumptions, not an end-to-end certification. README:30-54 explicitly trusts SVM rollback and wrapper authentication/oracle/custody. Wide arithmetic assumptions are discharged through differential/property fuzz tests; tests/proofs_v16_arithmetic.rs:164-182 uses small bounded symbolic inputs for one arithmetic comparison. Cargo.toml:41-47 enables stubbing and unwind 70. I did not run Kani or tests, so report source evidence, not a passing verification result.
13. Documentation inventory is stale: README lists 308 plain test proofs and 61 contracts; actual anchored attribute counts are 317 tests/proofs_v16.rs, 12 tests/proofs_v16_arithmetic.rs, 42 src/v16_proofs.rs and 62 contracts. spec.md:9 refers to scripts/no-steal-theorem.md, which is absent from this shallow checkout (git tree lacks it). This does not invalidate the existing proofs; it does invalidate using prose counts/artifacts as independent certification.

KNOWN ISSUES / DESIGN TRADEOFFS TO PRESERVE IN THE REPORT
14. Spec explicitly acknowledges a non-monotone terminal payout boundary: with zero source backing a claim enters junior receipts; with dust source backing it can realize dust and refine remaining face away to other junior claimants. More source backing can therefore reduce that claimant's total payout. Exact per-account payout can depend on realization timing vs expiry, despite snapshot conservation/order invariance. This is a documented design choice, not my new exploit claim. Avoid adopting it for Sakura's transparent capped-payout product.
    spec.md:1175-1177.
    https://github.com/aeyakovenko/percolator/blob/37cb14ee1436eaf3b7d97e1901b0dc1bf38f5311/spec.md#L1175-L1177
15. Open upstream issue #243 reports repeated Live refresh/liquidation failure when backing expires before a winner records the source domain. Report reproduced on its named earlier SHA pairs; I did not execute its PoC. Pinned current code still has the reported structural sequence: first_lapsed_source_backing_for_account_at_slot scans only existing source roster (:9891-9924); refresh scans before K/F attribution (:14678-14692); asset-level scan only used in CloseResolved (:16426). Therefore this is a significant unresolved integration concern with matching static evidence, not a fresh independently confirmed exploit at current HEAD.
    https://github.com/aeyakovenko/percolator/issues/243
16. Issue tracker also lists #176 liquidation choice dependent on physical/taker batch order and #207 terminal scan cursor skipping provider receivables. These were reported on earlier revisions. Do not automatically assert present vulnerabilities from open status; re-verify current code and regression tests before integration. They illustrate that arithmetic proofs do not imply economic fairness or liveness.
    https://github.com/aeyakovenko/percolator/issues/176
    https://github.com/aeyakovenko/percolator/issues/207

META: WHAT TO LEARN
17. Meta is a noncustodial governance bootstrap and reward system: fixed pre-existing COIN supply; owner-bound insurance/backing; constrained controller; surplus TWAP buybacks; read-only reward attribution. Governance should choose lifecycle/policy without holding a raw withdrawal capability over depositor principal. Immutable deployment is a stated PRECONDITION, not a promise enforced by an ordinary README. Upgrade authority can replace any restriction; collateral freeze authority can halt withdrawals.
    README.md:3-44,155-171.
    https://github.com/aeyakovenko/percolator-meta/blob/d1a147cc36d3302fc2115d77a8adae7b4fa87241/README.md#L29-L44
18. Signed intent must bind account incarnation, principal and policy. Subledger full exits encode expected principal and last-deposit/start slot. An older amountless exit could be held by a relayer, then close a later larger top-up and permanently retire that owner's one-per-owner position. Fixed owner destination prevented theft but did not prevent forced premature exit/identity loss. Current require_full_exit_snapshot compares both fields before deriving amount.
    subledger/src/lib.rs:2298-2328; SECURITY_LOG.md:14347-14364.
    https://github.com/aeyakovenko/percolator-meta/blob/d1a147cc36d3302fc2115d77a8adae7b4fa87241/subledger/src/lib.rs#L2298-L2328
19. Bound policy must not change beneath a signature. Controller prevents trade-fee increases because pinned trade wire has caller fee floor, not max. It prevents extending previously nonzero stale/force-close deadlines. No caller-controlled recipient/amount in permissionless terminal return; payout goes to depositor-owned clean token account. New capital is share-priced after impairment and full-loss share generations prevent old claims consuming fresh recapitalization.
    market-controller/src/lib.rs:532-581; subledger/src/lib.rs:2331-2349,2584-2609.
    https://github.com/aeyakovenko/percolator-meta/blob/d1a147cc36d3302fc2115d77a8adae7b4fa87241/market-controller/src/lib.rs#L532-L581
20. Meta refuses positive live insurance payouts while the asset still has positions or unresolved loss state. Aggregate balance alone cannot prove it is free to withdraw; a stale loser may recognize loss only after insurance is gone.
    percolator-accounting/src/lib.rs:516-549; subledger/src/lib.rs:2608-2609.
    https://github.com/aeyakovenko/percolator-meta/blob/d1a147cc36d3302fc2115d77a8adae7b4fa87241/percolator-accounting/src/lib.rs#L516-L549
21. Reward program reads principal/portfolio state but can only move reward COIN; reward bug cannot confiscate principal. Funding-paid/volume/residual counters remain farmable with self-owned payer/receiver or manipulable oracle. The design explicitly requires vetted oracle markets and acknowledges weak age weighting is insufficient.
    residual-distributor/DESIGN.md:35-48,102-127.
    https://github.com/aeyakovenko/percolator-meta/blob/d1a147cc36d3302fc2115d77a8adae7b4fa87241/residual-distributor/DESIGN.md#L35-L48
22. Meta inconsistencies/reproducibility limits:
    - Cargo.toml:10-11 pins core 143e68c4917ed0400a27b952f036a5677047cd84 and wrapper da64be639168b496833dd4c701607a94fcb06b6c through file:///home/anatoly/percolator-prog. A clean external build cannot resolve this local path as-is. Never infer current core compatibility.
    - README:173-178 says pinned wrapper atomic batches reject any nonzero backing utilization fee; controller clears/rejects those policies, and trade fee increases require upstream maximum-bound change.
    - audit.md:5-11,15-55 cites program/, governance/, and program/tests paths absent at this HEAD. Treat as historical notes, not current audit certification.
    - residual-distributor/DESIGN.md:51-56 says principal/surplus can exit ANY TIME and always walk; current deployed positive-payout guard subledger:2608-2609 refuses while exposed. Current README:112-114,150-151 explains restriction. Do not market unconditional exits based on older design prose.
    https://github.com/aeyakovenko/percolator-meta/blob/d1a147cc36d3302fc2115d77a8adae7b4fa87241/Cargo.toml#L5-L11

SAKURA COMPARISON (READ-ONLY /Users/freemellr/code/sakura-perps)
23. Sakura is presently isolated-margin, oracle-and-pool, one position per owner/market, with LP-backed capped payouts. profit_cap_usd and settle_close cap gross payout at deposited collateral + reserved pool quote; negative equity pays zero; fees cannot exceed collected payout. Pool reserve ratio uses exact rational product comparison. Already fulfills the principal transferable solvency lesson without needing junior PnL liens or B-index socialization.
    crates/risk/src/position.rs:477-555; crates/risk/src/pool.rs:184-237.
24. Open/close ledgers keep trader locked collateral, LP equity and protocol fees distinct and exactly conserve quote atoms; reserve is an encumbrance on LP equity, not an extra liability. Bad debt is a diagnostic counter, not a claim silently debited from other users. Preserve this accounting model; do not mix Percolator principal-protected matched counterparties with Sakura LP counterparty risk.
    programs/sakura-perps/src/position.rs:360-448,507-553.
25. Close/liquidation fee, maintenance margin and spread already snapshot at open, protecting existing positions from later market retuning. Only signed opening policy remains worth investigating: OpenPositionParams:188-210 binds collateral, size, side and limit price but no fee maximum; open fee reads live market.open_fee_bps (:795), and set_risk_params may raise it (market.rs:644-645,758). A previously signed open can receive a larger fee if new collateral still passes initial margin. Proposed fix: explicit max_open_fee_quote and/or exact accepted risk-policy hash/version, with maximum total fees and expected cap in SDK quote. Review compatibility before changing ABI.
26. LP withdrawal delay is initialization-only in current setters (pool.rs:1190-1207 only updates max AUM/utilization). Therefore mutable-delay attack is NOT a demonstrated Sakura issue. Withdraw minimum output protects fees/output, per-position exit fees already snapshot.
27. Sakura closes/deposits use monotone NotWorsened utilization checks after a ceiling reduction instead of absolute gate, preserving recovery. This is the same essential liveness discipline Percolator's open issue demonstrates. Test adverse sequences and reachability, not merely final balance equations.
    programs/sakura-perps/src/pool.rs:616-636,1209-1223.

THREE TRANSFERABLE PRINCIPLES, ORDERED FOR IMPLEMENTATION
A. Signed economic covenant: bind quote/trade intent to exact market policy + position incarnation + explicit maximum total fee, minimum output, expiry and replay nonce. An owner signature is not unlimited future authority. Focus now on Sakura open-fee policy race; extend to future strategy automation.
B. One atom, one funded obligation: never let an unrealized PnL number, reputation score or strategy reward finance a new obligation without reserved quote. Preserve exact pool/trader/fee ledgers and reserve caps; new community underwriting must get a separate funded vault/domain and cannot raid the global LP reserve.
C. Separate reward/strategy/lifecycle authority from custody, with bounded owner-controlled exits. Delegated strategy can place risk-bounded orders, never transfer followers' collateral. Restrictive oracle/policy mode must still permit defined risk reduction/recovery. Reward registries bind market/position generation and cannot mutate principal.

DIFFERENTIATION DIRECTIONS (DESIGN PROPOSALS, NOT IMPLEMENTED OR CLAIMED WORLD-FIRST)
- Strategy covenants: a creator publishes an immutable recipe + version; each follower signs their own enforceable budget, max position/notional, fee/slippage, allowed markets/oracles and expiry. A relayer coordinates execution across individual isolated accounts, while followers retain custody and revocation. The social object is an auditable risk contract with a provenance-linked after-fee track record, rather than a creator's discretionary shared wallet.
- Community underwriting cells: opt-in, separately funded reserves underwrite only one recipe/version/market family up to a visible cap. Every fill exposes guaranteed max payout/capital-at-risk and its backing cell. Failed strategies can exhaust their own disclosed risk budget without cross-community subsidy. This requires new on-chain custody/accounting and proof/test work; a dashboard label alone would not implement isolation.
- Earned strategy attribution: creator rewards draw only from separately funded rewards or explicit follower-consented fees, vest against realized net outcomes, and cap to the same position generation. Reputation combines after-fee realized return, drawdown and covenant compliance; do not pay raw volume, gross PnL or arbitrary oracle-market counters. Without Sybil and incentive analysis this is a candidate design, not a solved mechanism.

RECOMMENDED ADVERSARIAL ACCEPTANCE TESTS
- Fee policy changed after user signs quote: exceed signed fee/policy -> atomic rejection; exactly equal -> success.
- Withhold old delegated close, reopen/top-up same address: old generation/nonce must fail without affecting new account.
- Oracle unavailable or restrictions tightened: new risk blocked, authenticated risk-reducing/terminal routes reachable.
- Same economic portfolio with different physical slot/batch order -> same liquidation choice and total fee.
- Funding/settlement split into many calls vs one call -> bounded documented rounding difference, no reward farming.
- Fresh community funds cannot cover previous generation's exhausted claims; reward bug cannot debit principal.
- Failed multi-step instruction leaves account/vault/reward state unchanged; pending loss cannot vanish when a position is closed/reinitialized.

END. Parent can use these notes as research input; they are not an independent security audit or executed verification result.
