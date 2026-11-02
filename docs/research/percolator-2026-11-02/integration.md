# Integration review notes

Contemporaneous source-review notes. See the [integrated report](../2026-11-02-percolator-and-risk-rooms.md) for final implementation and validation status.

Percolator integration research for Sakura — 2026-11-02

Method and scope
Read-only source inspection, fresh shallow clones of the four requested branches under /tmp/sakura-percolator-research. No upstream scripts, install commands, builds, probes, transactions or deployments executed. GitHub issue bodies/statuses were read through the GitHub connector after web-source review; connector status supersedes stale search/page caches. Findings below distinguish inspected code, upstream reported results, and design recommendations. This is not an audit or a successful test-run claim.

Snapshot manifest (all four LICENSE files are Apache License 2.0)
- percolator-prog main: 5cb331dde354517c6371a8acf92cecb194f3bb73; commit dated 2026-09-22. https://github.com/aeyakovenko/percolator-prog/commit/5cb331dde354517c6371a8acf92cecb194f3bb73
- percolator-match master: 60aac3a996d0264c47fbefe9a2625d6ee6fb6a47. https://github.com/aeyakovenko/percolator-match/commit/60aac3a996d0264c47fbefe9a2625d6ee6fb6a47
- percolator-cli master: 7f9004768e5f6cbc4fb8c08fc1e5a8751669d990. https://github.com/aeyakovenko/percolator-cli/commit/7f9004768e5f6cbc4fb8c08fc1e5a8751669d990
- percolator-stress-test master: 9aaca144dae7d826ee9fbe6699eecb9955a1290c. https://github.com/aeyakovenko/percolator-stress-test/commit/9aaca144dae7d826ee9fbe6699eecb9955a1290c
- Program's actual engine dependency is 4db11a8cb0053815e23a35d3a7d3edc265d8d866, not arbitrary engine HEAD: percolator-prog/Cargo.toml:58,63.
  https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/Cargo.toml#L58
- Program README:3-5, matcher README:3-5, CLI README:15-19 explicitly call the software unaudited and educational, with no production/real-funds recommendation.

High priority findings

1. Exact pinned upstream state has reported release blockers; fork-and-deploy is not justified.
   GitHub #451 is OPEN, authored by aeyakovenko, and names wrapper 5cb331dd / engine 4db11a8c. In resolved mode WithdrawBackingBucket requires zero portfolios and capital, but haircut receipt finalization can wait for fresh backing expiry. An unused, unencumbered backing amount is then forfeited at expiry. The report's 1-atom remaining receipt blocks 100,000 atoms of fresh backing until expiry. Source inspection confirms the wrapper gate at src/v16_program.rs:10625-10635 and fresh-expiry check at :10652-10657; the full exploit/terminal accounting outcome is an upstream report not locally reproduced.
   https://github.com/aeyakovenko/percolator-prog/issues/451
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L10625
   Lesson: test bounded exits for every fund owner independently of other users' cooperation; conservation alone does not prove rightful attribution or liquidity.

   GitHub #458 is OPEN and also names 5cb331dd / 4db11a8c. It reports signed funding floor applied per accrual call at low prices: at $0.03, per-slot cranks credit 8,000 atoms where the configured cap permits about 30. This is a conserved transfer between sides, so aggregate solvency checks miss it. A candidate older branch yields intended engine-level values but has unresolved rebase conflicts; wrapper test on fixed engine, several-account/partial-settlement cases, and Hybrid/EWMA profiles were explicitly not verified.
   https://github.com/aeyakovenko/percolator-prog/issues/458
   Lesson: rational-reference checks across low prices, both signs, amount scales, and crank partitions; preserve remainder at the appropriate state scope.

   GitHub #457 is CLOSED completed 2026-10-27, but closure is NOT evidence the requested main pin is repaired. Its final comment reports fixes on engine 4db11a8 + open PRs #208–#218 + #226 fd78ec1c, with a wrapper branch behind #443–#447. It explicitly did not test #226 alone on wrapper 5cb331dd, Kani, upstream wrapper suites, or CU. Reported symptom was partial source-backing rate burning winners' face on a reversal before losers settled, leaving value unattributed; a second give-back case occurred even with all accounts settled. The combined fix reportedly restores the healthy example but leaves other cases out of scope. Do not present this as an open issue, a default-main fix, or our reproduction.
   https://github.com/aeyakovenko/percolator-prog/issues/457
   https://github.com/aeyakovenko/percolator-prog/issues/457#issuecomment-6143110564
   Lesson: replay all settlement orders, source-backed face netting, one-maker-many-takers workloads, bankrupt variants, exact terminal payouts, and attribution before adopting fixes.

   Two further PRIMARY REPORTS: #438 is explicitly HARDENING, not demonstrated value-moving exploit: live asset-insurance withdrawal checks the long loss barrier before debiting long and short budgets; author says global counters may cover reachable states. #436 reports default anchor-v2 account-growth failure beyond 10KiB while legacy-entrypoint tests pass. These were inspected as test-design lessons, not reproduced or asserted unpatched in this exact pin.
   https://github.com/aeyakovenko/percolator-prog/issues/438
   https://github.com/aeyakovenko/percolator-prog/issues/436
   Lesson: distinguish reachable loss proof from asymmetric-code suspicion; verify exact production feature set and SBF artifact, not only legacy/native builds.

2. CLI default branch is NOT a compatible current SDK.
   percolator-cli/src/v16/instructions.ts:263-276 serializes TradeCpi as only tag, assetIndex, sizeQ, feeBps, limitPrice. Current program src/v16_program.rs:9238-9248 also expects both portfolio IDs and position epochs, maker matcher sequence, asset market_id, backing fee cap. CLI SetMatcherConfig :570-571 emits only tag+enabled; current handler :9480-9486 takes portfolio ID, sequence, position epoch, generation frontier, enabled, fee cap, expiry. CLI parser comments pin old dump_layout at 70294cb (parsers.ts:1-4), and constants.ts:96 asserts a 64-market ceiling contradicted by current dynamic wrapper. Treat CLI as historical wire/reference examples, never a direct production dependency.
   https://github.com/aeyakovenko/percolator-cli/blob/7f9004768e5f6cbc4fb8c08fc1e5a8751669d990/src/v16/instructions.ts#L263
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L9238
   Lesson: generated ABI + pinned engine/wrapper/matcher manifest + encode/decode fixtures from exact artifact. Reject unknown layouts, owner IDs, program hashes and account sizes before displaying authoritative balances or signing.

3. Matcher source and current wrapper have a partial-fill compatibility gap.
   Current matcher vamm.rs:596-604 and :653-661 cap fills by max_fill/inventory; successful nonzero paths at :642 and :719 return FLAG_VALID only. Current wrapper src/v16_program.rs:4672-4675 rejects any partial without FLAG_PARTIAL_OK. Thus a capped nonzero matcher fill appears to fail under the current wrapper (source-based inference; not runtime tested). Full batches require exact requested sizes even with PARTIAL_OK (:4696-4700) because there is no signed residual strategy budget.
   https://github.com/aeyakovenko/percolator-match/blob/60aac3a996d0264c47fbefe9a2625d6ee6fb6a47/src/vamm.rs#L596
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L4672
   Matcher vamm.rs:500-552 explicitly carries ONE inventory_base scalar across heterogeneous asset legs; asset_index is echoed but pricing/inventory calculation does not choose asset-specific state. This is a simplistic LP policy, not an asset-normalized portfolio AMM. One asset-specific context per venue or per-asset keyed inventory is necessary for a community multiasset product.
   The README's fixed ±50bps and 1-byte init narrative is stale relative to routed vAMM initialization; lib.rs:242-245 routes tag 2 into vamm::process_init which parses expanded params. Also README promises ceil ask but deployed compute_passive_execution :626-630 uses floor integer division (old passive helper has different rounding). Audit actual entrypoints, not exported legacy helpers.

Trust boundaries worth preserving

4. Risk core is pure accounting, not authorization.
   Program README:282-304 separates pure risk engine (no CPI, token transfer, signature/ownership checks) from trusted wrapper (auth/custody/oracles/ABI) and LP-scoped matcher. Engine relies on Solana whole-transaction rollback; standalone *_not_atomic calls can leave partial state on errors. A simulator MUST snapshot all touched state and restore on ANY error, including late instruction/CPI failure. An application that uses engine functions is not thereby a secure Solana program.
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/README.md#L282

5. Matcher is LP-authorized, never a protocol authority.
   Wrapper :9258-9273 checks taker signer, writable market/both portfolios/context, program ownership, distinct portfolios, executable matcher, nonexecutable context owned by matcher, minimum return length. :9275-9292 binds portfolio incarnation + position epoch + asset generation before CPI. :9340-9372 derives delegate from market, LP portfolio, LP owner, matcher program/context and checks LP stored enabled config, sequence, fee cap, expiry. :9202-9230 rejects signer-bearing tail accounts, protected account aliases, wrapper program and wrapper-owned accounts.
   :9396-9421 increments a persisted matcher request sequence and invokes with scoped PDA seeds. README:333 incorrectly says no persisted market nonce; actual source is authoritative. Return validation :4629-4677 rejects wrong ABI, unknown flags, rejected/unbound echoes, zero price, reversed/overfill sizes, i128::MIN and unflagged partials. :9435-9443 enforces the taker's price limit only when nonzero. :9463 uses executed size, not requested size. Matcher selection is not a price-quality guarantee; signed nonzero limit and fee caps are essential frontend defaults.
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L9202
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L4629
   Matcher lib.rs:263-279 and :295-323 verify context ownership/length/initialization and PDA signer, then vamm.rs:461-464 binds its stored LP PDA. Init vamm.rs:353-427 is one-time and checks owner/writable/size but does not require an LP signature. Atomic creation+init avoids front-running uninitialized context (CLI README:611 explicitly warns).
   https://github.com/aeyakovenko/percolator-match/blob/60aac3a996d0264c47fbefe9a2625d6ee6fb6a47/src/lib.rs#L263

6. Custody authenticity includes canonical address, not merely owner.
   Wrapper :16208-16226 allows only executable classic SPL Token program, token owner SPL Token, and exact classic account size. User token verifies mint, owner, initialized state. Vault :16299-16317 additionally rejects delegate/close authority and requires canonical ATA derived from market vault authority + mint, preventing fragmented vault deposits that strand withdrawals. This is not generic Token-2022 compatibility; do not accept transfer-fee/hook/rebasing collateral by UI promise. Withdrawal destination validation :16263-16296 also checks selected approved quote mint and optionally unencumbered recipient.
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L16208

7. Oracle authentication != market truth; each mode has different authority.
   Wrapper :5129-5167 dispatches only fixed Pyth Receiver / Switchboard On-Demand mainnet or devnet / Chainlink store owners. Pyth :4904-4931 checks length, discriminator, FULL verification, exact feed ID, positive price/bounded exponent, freshness and confidence. Freshness :4838-4844 rejects future as well as stale publish times. Confidence conf_bps=0 disables confidence filtering (:4859-4862); admission should forbid accidental disabled filtering for real markets. Switchboard :4999-5029 validates sample count/minimum, slot, update/publish time, positive value, stddev/confidence. Chainlink :5090-5125 pins feed account, discriminator, nonzero version/round/slot, single live transmission and freshness; do not imply equivalent confidence metadata across providers.
   Composite leg timestamps must be exactly equal (:5264-5296), not merely individually recent: an honest multi-feed setup may fail liveness without synchronized publication. Oracle uptime/sampling assumptions must be tested, not glossed over.
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/src/v16_program.rs#L4904
   Trade paths consume already-stored effective mark; they do not fetch a fresh oracle on every trade (README:468-476). AuthMark is authority-pushed direct mark (README:590-598), including adapter PDA option; EwmaMark is authority mark/index with smoothing/funding (600-608); Hybrid uses external when fresh and EWMA after controlled staleness, with dynamic fees (610-639). A custom/community mark is centralized to its configured authority unless a real independently verified aggregation adapter exists. Smoothing and fees do not prove manipulation resistance.
   Per-slot effective-price cap needs keeper cadence; README:715-717 explicitly says it does not guarantee safety when keepers disappear. Surface raw target, effective price, age, lag, confidence, mode and keeper progress separately.

8. Governance scope is economic risk, even without direct theft permission.
   README:881-920 describes marketauth policy changes, fee/base-mint controls, force shutdown/resolve, and initial asset-0 admin ability to rotate oracle/insurance roles. Per-asset role separation and burning asset_admin constrain some powers; marketauth still exists. A community vote or creator badge must not conceal upgrade authority, mark authority, insurance operator, shutdown authority, or fee changes. Make market manifest versioned; timelock economic changes and give executable exit windows where supported. Community-specific insurance and collateral isolation are valuable, but not proof no cross-asset loss; verify engine guarantees and scope user opt-in.

Verification coverage and limits

9. Wrapper has substantial hostile-boundary tests but scoped proofs, not whole-system certification.
   README:828-853 says Kani proves decoding, malformed/trailing bytes, matcher returns, funding clamp/sign. It does NOT model full CPI or internal accounting. Owner/signer enforcement, transfers and economic/liveness behavior depend on LiteSVM integration plus engine proofs. tests/invariants/README.md:3-41 shows a focused maintenance retry regression explicitly does not close the broader #386 cadence question. Lines 43-79 record recovery cleanup failed to advance position epoch, a previous adjacent test missed it, then a bounded fix/test; lines 78-79 disclaim general liveness closure.
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/README.md#L828
   Hostile matcher tests craft overfill, reversed direction, forged asset/oracle/request/LP echoes, zero price and unflagged partials (tests/fixtures/hostile_matcher/src/lib.rs:107-151). INV-019 includes stale/no-write response replay, context/portfolio recreation, single/batch and nested return-data paths. These are useful patterns, not proof arbitrary plugins are safe.
   https://github.com/aeyakovenko/percolator-prog/blob/5cb331dde354517c6371a8acf92cecb194f3bb73/tests/fixtures/hostile_matcher/src/lib.rs#L107

10. Stress repository results are historical, configuration-specific and engine-only.
    Cargo.toml:12 depends on local ../percolator with stress feature, no revision. audit.md:3-4 is v12.14.0, 7000 runs, 0 solvency violations; not current v16 and not independent audit. scripts/v16_stress_summary.md:3-6 targets a37eb5f on 2026-05-18; scripts/v16_bugfix_resweep.md:1-4 targets 272213f. src/main.rs starts with stale v14 port-progress text, but lines 31-44 describe a v16 snapshot shim. No unique tested engine can be inferred from master alone.
    Current shim's assert_public_invariants (:675-693) is shape check + vault >= capital+insurance + matured <= positive pnl, not a complete engine invariant suite. Battery :1081-1146 adds K/F/A bounds, negative-PnL counts, capital sum, reserved claims and side-mode conditions; may miss rightful payout and settlement-order fairness. Some directed probes print errors/results instead of hard assertions; aggregate (:1404-1445) prints invariant failure totals but does not fail nonzero there. CI must convert every violation into failure, not rely on green process exit.
    https://github.com/aeyakovenko/percolator-stress-test/blob/9aaca144dae7d826ee9fbe6699eecb9955a1290c/Cargo.toml#L12
    https://github.com/aeyakovenko/percolator-stress-test/blob/9aaca144dae7d826ee9fbe6699eecb9955a1290c/src/main.rs#L675
    scripts/v16_critical_finding.md:3-5 targets 4f1ff20 PLUS uncommitted WIP; :93-101 corrects earlier false-positive extraction by distinguishing paper PnL from actual cash and restoring atomic rollback. Important methodology lesson, not a current security claim. Its narrative describes blocked/trapped attacker state, so no extraction does not imply general liveness. README:7-10 also distinguishes A/K fairness from haircut fairness over the stored realized claim set; neither implies global fully settled fairness.
    https://github.com/aeyakovenko/percolator-stress-test/blob/9aaca144dae7d826ee9fbe6699eecb9955a1290c/scripts/v16_critical_finding.md#L93

Concrete Sakura opportunities (proposals, not externally proven novel inventions)

- Community market manifests: commit-addressed policy with collateral/feed identities, oracle mode/confidence/staleness limits, admin/upgrade authority, matcher identity, fee caps, leverage/open-interest limits, insurance/backing reserves, exit and listing review state. Show these as user consent and machine-checked admission conditions.
- Conservative first product: isolated community markets and a deterministic paper-trading / security workbench, with real-money routes disabled until exact engine-wrapper-SDK-matcher quartet passes artifact-matched tests and independent review. Community membership/reputation can organize markets but must not substitute for collateral or oracle verification.
- Risk visibility as product: distinguish deposited capital, mark PnL, source-backed credit, haircut-adjusted claim and actually withdrawable funds; visualize how stale targets/keepers affect those numbers. Do not label all positive PnL withdrawable or guaranteed.
- Safer execution: scoped expiring typed intents (program/cluster/market-generation/portfolio-incarnation/position-epoch/nonce); nonzero signed limit; fee cap; minimum fill; explicit full-or-nothing ratios for spreads; preview exact fees and margin after trade; consume nonce only after atomic success. Per-community/per-asset inventory and quote-size caps.
- Contributor-built adversarial scenarios: malicious matcher, stale/future/wide-confidence oracle, composite timestamp mismatch, expired/retained grants, same-address reincarnation, duplicated batch assets, alternate tail signer drain, underfunded/dust liquidation, insurance depletion, keeper outage, late-transaction rollback, and resolved-provider exit.
- Differential sequence testing: same economic operations under chunked vs single funding accrual, all crank permutations, owner rotations/restarts, exact price/amount extremes and multi-leg partials. Independently reconcile every token atom to user capital/fees/backing/insurance/residual/terminal payout, including rights and liveness rather than mere balance non-negativity.
- Realistic moat is verifiable market curation, transparent loss/exit economics, maker/keeper quality, reproducible manifests and incident rehearsal. Fixed spreads, a vAMM, community tokens, cross-margin or cosmetic social feeds alone are not new risk-engine research. Avoid adding recursive collateral, unrestricted creator-pushed marks, cross-community margin netting or unbacked reward points during the initial safety boundary work.

Recommended acceptance gate before any real-funds build
Pinned source+dependency lockfiles and Apache notices; exact deployed code hash/feature profile; current generated client fixtures; known-loss reports triaged against the pin; pure model reference and atomic rollback; hostile matcher SVM tests; authentic oracle fixture matrix; full lifecycle and permissionless terminal exit with correct beneficiaries; low-price funding partition tests; compute/account resize worst-case on production SBF; external security review; controlled devnet exercise. These are engineering recommendations, not a request to deploy or an assertion the current research completed them.
