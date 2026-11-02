# Sakura Perps: security and community product research

Research date: 30 October 2026. Audience chosen by the owner: crypto-native
communities interested in teams, strategy and social play.

## Decision

Build toward **community strategy seasons with individually controlled capital,
explicit risk budgets and verifiable settlement rules**. Working name: Sakura
Clubs. Make the first community prototype paper-only, while a separate workstream
resolves the financial core's accounting and emergency behavior.

This is a product hypothesis, not an established invention. Teams, competitions,
bounded payoffs, perpetual options, simulated challenges and risk limits all have
prior art. The opportunity is their careful integration: a community can plan,
play and learn together, while every member can independently inspect and limit
the authority granted over their funds. Research did not establish worldwide
novelty, product-market fit or security certification.

## What exists in this repository

The reviewed source is commit `2bb0f71b12700e64dce172a9ddf4abc1ae51d258` on
`codex/perps-security-hardening`. [PR #3](https://github.com/Sakura-11488/sakura-perps/pull/3)
was still open and draft when checked on the research date. Its
[six CI jobs passed](https://github.com/Sakura-11488/sakura-perps/actions/runs/40969454367).
The recorded test totals are 17 keeper, 14 dashboard, 93 risk, 26 program host
and 91 integration tests; these are evidence about the tested cases, not a
probability of safety. The hardening changes have not been deployed. The latest
recorded devnet release remains `devnet-v0.7.0`, dated 26 August 2026.

The prototype already has isolated oracle-priced positions against a liquidity
pool, fees and accrual, liquidation and emergency-close paths, share mint/burn,
and a profit reserve. In `crates/risk/src/position.rs`, `settle_close` bounds
gross payout by collateral plus the position reserve, then deducts fees. Capped
exposure is therefore an existing mechanism to scrutinize, not a new feature
discovered by this research.

PR #3 adds two-step exchange-admin transfer, durable keeper submission and cost
accounting, fair scanning, truthful dashboard observations and CI regressions.
Admin transfer does not rotate the program upgrade authority. Keeper cost
reservations are estimates, not an onchain maximum transaction debit.

The key unfinished issues are explicit in [the security guide](../security-hardening.md):

| Gap | Consequence |
|---|---|
| LP share pricing uses `pool.quote_deposited` without marking open claims | LPs can enter or leave at an economically wrong valuation. The 20% reserve ceiling limits exposure; it does not establish fair pricing. |
| Emergency valuation uses `last_good_price`, including observations accepted under liquidation rules | A remembered price is not necessarily a fair crisis settlement price. Its age and provenance need an explicit policy. |
| Funding, reserve allocation, bad debt and keeper incentives need adversarial economic analysis | Passing accounting checks does not establish that rational adversaries cannot extract value or prevent progress. |
| Sustained funded-book operation is unproven | An empty-book preflight does not demonstrate price refresh, liquidation throughput or recovery under failure. |
| Dependency advisories and independent assurance remain | The current branch is not a mainnet release candidate. |
| Sakura's app currently uses Phoenix | This protocol still needs its own SDK and end-to-end product integration. |

The dependency counts in the security guide are snapshots from the hardening
change, not a fresh vulnerability scan for this memo. No mainnet deployment,
live trading test or comprehensive audit was performed for this research.

## What the market already has

Official documentation establishes that a mechanism is described. It does not
independently establish deployment correctness, adoption, solvency or safety.

| Idea | Primary evidence and status | Implication |
|---|---|---|
| Oracle-and-pool perps with reserves and profit limits | [GMX's implementation documentation](https://github.com/gmx-io/gmx-synthetics/blob/main/README.md) describes reserves, funding, price impact, pending PnL in pool valuation and PnL caps. [Jupiter's current reference](https://docs.jup.ag/user-docs/trade/perps/technical-reference) describes its pool-based system. | These mechanisms are established. Differentiate on demonstrated accounting, controls and experience. |
| Gamified capped trading | [Jupiter YOLO beta](https://docs.jup.ag/user-docs/labs/yolo) documents 500–1,000× leverage, adjusted profit capped at 50× collateral, a 72-hour limit, leaderboards and Double or Nothing. Profits can wait in a FIFO queue when the House cannot pay. These are documented launch settings. | A cap alone does not mean a payable obligation has been fully reserved. Playful branding and game modes are already occupied territory. |
| Team trading competitions | [Bybit's 29 October 2026 WSOT announcement](https://www.bybit.com/press/post/wsot-2026-is-live-registration-opens-for-bybits-world-series-of-trading-with-a-prize-pool-of-up-to-10-million-usdt-across-crypto-stocks-and-ai-bbb5ccce60c205407fb) describes squad and individual competitions, with rewards linked to participation and trading volume. | Squads and social rankings are established. Community identity alone is insufficient differentiation. |
| Paper perp seasons with rewards | [dreamDEX Perps Arena](https://app.testnet.dreamdex.io/competition) documents a four-week October–November 2026 testnet competition, paper balances, weekly/season ROI and real rewards. Its [perps guide](https://app.dreamdex.io/docs/trading/perps-testnet) explicitly says perps are testnet-only. | Practice mode, quests and seasons are already present elsewhere. |
| Teams with equal budgets and risk-aware rankings | [ICAIF 2026 Perpetual Alpha](https://icaif2026.org/competitions.html) describes a Hyperliquid testnet research competition with equal starting capital and performance, volatility and expert-assessment components. | This combination is prior art too; a research contest does not establish consumer demand or an ungameable score. |
| Simulated challenges with loss limits | [MyFundedPerps' September 2026 rulebook](https://myfundedperpetuals.com/blog/myfundedperps-rules-explained) describes equity-based daily and static loss rules, evaluation accounts and competition allowances. | Budget rules in a trading challenge already exist. Operator-administered limits do not establish protection of a live self-custodial wallet. |
| Fully collateralized bounded claims | [Hyperliquid HIP-4](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-4-outcome-markets) describes dated outcome contracts with bounded settlement and an initial mainnet market. Deployment was not independently checked here. | Fully funded bounded outcomes are established primitives; dated outcomes and perpetual futures are different contracts. |
| Exposure without ordinary price-triggered liquidation | [InfinityPools' mechanism](https://docs.infinitypools.finance/protocol-overview/introduction) uses borrowed concentrated liquidity; its [trading guide](https://docs.infinitypools.finance/welcome/trading) explains interest and closure when funding is exhausted. Current deployment/adoption was not independently established. | Removing one liquidation mechanism moves costs and termination conditions elsewhere. |
| Perpetual options | [Panoptic streamia](https://panoptic.xyz/docs/product/streamia) describes ongoing option premiums, with accumulated premium capable of triggering liquidation. [Everlasting Options](https://www.paradigm.xyz/writing/everlasting-options), published in 2021, is earlier conceptual prior art. | A new payoff or funding rule needs pricing and collateral analysis. The label does not confer safety. |
| Convex or playful bounded derivatives | [Power Perpetuals](https://www.paradigm.xyz/writing/power-perpetuals), published August 2021, and [Smilee's September 2023 paper](https://medium.com/smilee-finance/smilee-smoothpaper-v1-69-81a60a0bc391) describe nonlinear exposure and option-like products. [Squeeth's official site](https://squeeth.opyn.co/) records its November 2024 shutdown. | These are relevant historical mechanisms; do not present discontinued products as current operating comparators. |

Hyperliquid and dYdX also show why loss allocation must be explicit. Hyperliquid
documents an [HLP liquidation backstop](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations)
and [automatic deleveraging](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/auto-deleveraging).
dYdX's [23 April 2026 operator explanation](https://help.dydx.trade/en/articles/166973-contract-loss-mechanisms-on-dydx-chain)
describes negative-equity deleveraging differently from older community docs.
Version and deployment matter when borrowing another protocol's design.

## The proposed community experience

A community creates a season around a strategy question: for example, how to
manage SOL exposure through a volatile week. Members take complementary roles
such as analyst, risk reviewer and trader. They write a thesis, record an intended
risk envelope, compare decisions and review the outcome together.

The first version should support:

1. **Private clubs and team identity.** Invite links, a shared strategy board,
   optional public results and member-controlled visibility. A club role grants
   social permissions only; it grants no withdrawal or trading authority.
2. **Equal paper starting conditions.** Clearly labeled simulation, recorded
   prices and deterministic fills with explicit spread, fee, funding and latency
   assumptions. A plausible simulator must not offer free fills at prices a live
   participant could not obtain.
3. **A plan before the outcome.** Timestamped strategy submissions, budgets and
   optional delayed disclosure. Compare the plan with what happened. Delayed
   disclosure is a privacy/product experiment, not a proven MEV defense.
4. **Cooperative challenges.** Stress replays, explaining a hedge, identifying an
   unsafe market, or navigating an oracle outage. A member can contribute through
   research and risk review without placing a trade.
5. **An honest season review.** Show net marked performance, drawdown, costs,
   risk taken, budget changes and failed assumptions. Avoid a single number that
   implies a short lucky streak establishes durable skill.

After financial release gates are met, a separately opted-in live mode could use
an individually funded account for each participant. Team captains cannot move
someone else's money, increase their limits or quietly enable live trading.
The social service and any reward treasury must have no authority over trading
vaults. Compromising the leaderboard should not authorize financial actions.

The user-facing differentiator would be a **risk receipt** for every live
authorization: collateral committed, maximum authorized external contribution,
allowed markets and notional, execution-price bounds, fee/carry terms, payout
cap, reserved backing, expiry/revocation of delegated permissions, emergency
rules and upgrade powers. Each field must correspond to an enforced rule or be
clearly identified as information rather than enforcement.

Budget terminology is critical. A program can enforce a cap on collateral
contributed to a dedicated account, subject to its correctness and authority
model. It cannot guarantee that an ordinary stop order fills before a chosen
drawdown during a price gap. Reinvested profits, top-ups, gas, sponsored fees,
stablecoin depegs and issuer freezes need separate definitions. Express hard
account limits in token units; a USD display is not a guarantee of dollar value.
Top-ups must require a new explicit authorization and remain visible in the
season record. Session permissions should fail closed when stale or revoked.

Where the program remains upgradeable, an upgrade can change enforcement. A
receipt must expose that authority and any delay or exit protection; it cannot
promise immutable limits while an unrestricted upgrade key can replace them.

A season ending should end the competition's scoring window. It should not
silently convert a perpetual position into an expiring contract. If we choose
finite-duration, prepaid option-like exposure instead, that is a different
financial product requiring a separate specification and accurate naming.

### What would falsify this direction?

Run a small paper-only pilot across three existing communities and two seasons.
Treat those numbers as an experiment design, not statistical proof. Before the
pilot, record decision criteria for repeat participation, collaboration, correct
understanding of caps and fees, and willingness to continue without subsidies.
Interview people who stop participating as well as those who remain.

The hypothesis weakens if members only value rewards, volume farming or extreme
leverage; if team roles add no value over a chat and a trading screen; or if users
cannot distinguish simulated funds, committed collateral and a stop-loss target.
Find and compare any new competitor with the same integrated experience before
making a novelty claim.

Do not launch transferable points or financial rewards in the first experiment.
Sponsor-funded rewards, if later chosen, need a separately funded, bounded prize
pool and eligibility rules. LP assets must not implicitly fund a game treasury.
Commercial viability must separately establish that fees can cover operating
costs and LP risk compensation at acceptable execution quality. Durable demand
cannot be inferred from subsidized trading volume.

### Competition scoring is an adversarial surface

Sharpe, Sortino and similar metrics can be improved through strategies that do
not represent superior information; this is established in
[Goetzmann, Ingersoll, Spiegel and Welch's performance-measure research](https://repec.som.yale.edu/icfpub/publications/2471.pdf).
Calling a leaderboard risk-adjusted does not make it manipulation-proof.

Specify common marks, cash-flow treatment, unrealized losses, funding and fees,
entry deadlines, reset rules and disconnections. Test correlated accounts,
opposite bets, selective account abandonment, last-minute risk and concealed
open losses. Lock team rosters and scoring cohorts before rounds so teams cannot
discard losing members. Wallet count is not person count. Reward design should not depend
on trade count, turnover, leverage or recovering losses. Begin with explanatory
scorecards and nonfinancial recognition; any prize formula needs its own review.

An [FCA experiment reported in June 2024](https://www.fca.org.uk/news/press-releases/fca-keeps-trading-apps-under-review-over-gaming-concerns)
involved over 9,000 consumers and found that some engagement features increased
trading frequency and risk-taking. This is evidence from an experimental trading
app, not a measured Sakura effect. It supports testing enjoyment and retention
alongside risk behavior rather than treating more trades as product success.

## Security lessons that change the design

| Evidence | Required response for Sakura |
|---|---|
| [dYdX's January 2024 SUSHI/YFI postmortem](https://dydx.exchange/blog/sushi-yfi-incident) describes concentrated exposure, recycled unrealized gains and liquidations after negative equity. | Aggregate exposure and manipulation cost matter across wallets. Thin reference markets and per-wallet limits cannot be the foundation of containment. |
| [GMX's July 2025 V1 report](https://gmxio.substack.com/p/glp-funds-on-arbitrum-fully-recovered) describes cross-contract reentrancy and corrupted short accounting/GLP valuation; V2 was unaffected. | Verify accounting across every entry point and LP operation. This is an invariant lesson, not a claim that Solana has the same call semantics. |
| [Drift's 16 April 2026 recovery report](https://www.drift.trade/updates/incident-recovery-update-april-16-2026-now) describes approximately $295.7m affected and proposed signer, verification and timelock safeguards. Its [October recovery update](https://www.drift.trade/updates/dfx-claim-and-redemptions) references Velocity. | Operational authority can dominate contract-level controls. Relaunch commitments are not evidence those controls were implemented. |
| [Pyth's current best practices](https://docs.pyth.network/pro/best-practices) discuss latency, depth, exposure and availability alongside timestamp checks. | Fresh signed data alone is insufficient. Test delayed execution and market capacity. Pro-specific fields must not be copied into Sakura's Core receiver integration. |
| [Certora's March 2025 Kamino case study](https://www.certora.com/blog/securing-kamino-lending) found an exchange-rate rounding issue through invariant analysis, while qualifying its then-current exploitability. | Analyze extreme arithmetic and composed mint/redeem sequences. Verification needs documented assumptions and reachable states. |

An authenticated oracle may faithfully report a manipulated external market.
An available balance may back multiple incorrectly counted claims. A multisig
may approve a malicious transaction. A verified build may faithfully reproduce
a vulnerable program. Each requires distinct evidence.

## Financial and engineering work, in order

### Gate 1: executable accounting and LP fairness

Write an independent reference model of vault assets, trader collateral,
position equity, profit caps, funding, borrowing, fees, LP equity and withdrawal
claims. Define units, rounding and time treatment. Compare it against compiled
SBF behavior, not only against the same Rust functions it is supposed to check.

Preserve the distinction between maximum reserved payout and current fair
valuation. Reserves are claims against LP capital, not an additional asset or a
second liability layered onto that same capital. Subtracting every reserve from
NAV is not a valid shortcut. Likewise, per-position clipping does not distribute
over net aggregate PnL; [the M5 specification](../m5-spec.md) already identifies
this obstacle.

Compare two explicit designs: continuously priced LP entry/exit with complete,
consistent liability valuation; and separately funded underwriting tranches
with honest lockup/redemption terms. A tranche cannot promise a finite exit
while its perpetual obligations remain outstanding without a specified transfer
or settlement mechanism. Choose through proofs and operating constraints.

Acceptance: no double use of reserves; no payout beyond the contract's funded
maximum; no value extraction through deposit/withdraw/settlement order under an
identical valuation snapshot beyond documented rounding and charges; and exits
that remain possible under the defined degraded-state policy. Do not interpret
this as order independence across genuinely different prices or timestamps.

### Gate 2: oracle and crisis state machine

Specify normal trading, restricted trading, quarantine, emergency settlement and
recovery. For each state, define who can trigger it, what evidence is needed,
what actions remain available, and how existing positions are treated.

Separate strict settlement observations from deliberately looser liquidation
observations. Cover stale, future-dated, out-of-order, divergent and revoked
feeds; decimals and overflow; confidence boundaries; outages and recovery.
Decide what happens when no defensible price exists. Neither a timer nor entry
price automatically produces a fair exit. Test conflicts between rapid access
to funds, manipulation resistance and fairness explicitly.

Consider delayed or batched entry only after modeling latency and UX costs.
[Frequent batch auctions](https://www.econ.umd.edu/publication/high-frequency-trading-arms-race-frequent-batch-auctions-market-design-response-0)
are established market-design research, not a new Sakura invention or a blanket
solution to oracle latency, censorship or missing keepers.

### Gate 3: economic containment and liveness

Simulate asymmetric books, reserve exhaustion/grief, funding farms, delayed
accrual, bad-debt recovery, correlated accounts, LP exits, depegs and price gaps.
Include shocks worse than observed history and report sensitivity to liquidity
and actor assumptions. Determine who funds a liquidation whose remaining
collateral cannot reward the keeper.

Specify aggregate market/side exposure, growth rate and capital-at-risk limits.
Start with one liquid reference asset and isolated risk allocation. Any later
shared backing between markets needs an explicit contagion budget. Model the
capital-efficiency cost of reserving maximum payout, the price of optionality,
and LP compensation. Lower leverage alone does not resolve accounting errors.

Funding is part of the economic contract. [A Primer on Perpetuals](https://arxiv.org/abs/2209.03307)
and [Perpetual Futures Pricing](https://arxiv.org/abs/2310.11771) provide models
whose assumptions must be checked before reuse. An endless exposure with
bounded capital, no forced close and unlimited upside cannot simply be declared
free of financing or underwriting costs.

### Gate 4: stateful testing and formal properties

Run generated sequences across opening, accrual, close, liquidation, LP mint and
redemption, oracle changes, pauses, admin transfer and upgrades. Include hostile
accounts and transaction composition. Retain minimized failing sequences as
regressions. Formalize arithmetic, conservation, authorization, reserve
exclusivity and rejected-call rollback.

[Solana Prover](https://docs.certora.com/en/latest/docs/solana/index.html) is a
candidate tool; use its [vacuity checks](https://docs.certora.com/en/latest/docs/solana/sanity.html)
or equivalent reachability checks. Publish assumptions and exclusions. A proof
of a restricted model does not establish unmodeled economic or operational safety.

### Gate 5: operational and authority rehearsal

Demonstrate a funded devnet book with successful pinned-feed updates and actual
liquidations. Measure worst-case scan and execution delay at the intended book
size during RPC loss, congestion, budget exhaustion, restart and ambiguous
confirmation. Validate independent keeper operators before relying on redundancy.

Inventory exchange admin, program upgrades, feed qualification, listing limits,
treasury, frontend and CI powers. Test multisig and timelock integration with
independent transaction decoding, dedicated signing devices and narrow emergency
permissions. Delayed risk increases and fast risk reduction need separate rules.
Review approval expiration: [Solana durable nonces](https://solana.com/docs/core/transactions/durable-nonces)
remove ordinary recent-blockhash expiry. Two-step admin transfer alone does not
solve signer compromise or upgrade authority.

### Gate 6: independent release evidence

Obtain independent Solana implementation, economic-risk and operational reviews;
fix and retest critical/high findings. Re-evaluate dependency advisories and
reachable impact. Record exact source, compiler, binary hash, parameters and
authorities. [Verified builds](https://solana.com/docs/programs/verified-builds)
establish deployed-byte identity, not security.

Rehearse oracle outage, malicious admin proposal, compromised signer and
emergency settlement. Prepare funded response and disclosure procedures. Any
real-value pilot needs deliberately small exposure caps and evidence-based
expansion criteria. Access, derivatives and competition requirements must also
be assessed for the actual launch jurisdictions before real-money operation;
this memo does not make a legal determination.

## Concrete next deliverables

**Financial workstream:** an accounting RFC, a small independent executable
reference model, a catalog of adversarial sequences, and a comparison harness
against the existing compiled program. Resolve LP valuation and emergency
settlement before increasing limits or adding payoff complexity.

**Product workstream:** a paper-only Sakura Clubs prototype with team roles,
strategy submissions, risk receipts, simulated positions and replay reviews.
Validate whether communities enjoy that loop without prizes or required volume.

Keep both workstreams inspectable. Security progress means a specific property
with evidence; product progress means observed understanding and repeat use.
The implementation, economic model and operating system each need to earn their
claims independently.
