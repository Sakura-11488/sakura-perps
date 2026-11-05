# Sakura Perps roadmap

Recorded 5 November 2026. These stages are acceptance gates, not launch dates or
completion percentages. The product hypothesis is that communities return to
plan and learn together while each member retains control over their decisions.

Track the [current-phase milestone](https://github.com/Sakura-11488/sakura-perps/milestone/1): [cohort sessions](https://github.com/Sakura-11488/sakura-perps/issues/7), [off-host recovery](https://github.com/Sakura-11488/sakura-perps/issues/8), and [reporting and branch protection](https://github.com/Sakura-11488/sakura-perps/issues/9). The [realistic-paper specification](https://github.com/Sakura-11488/sakura-perps/issues/10) is planned separately.

## Current baseline

The invite-only [Community Rooms paper beta](https://sakuraonseeker.com/app/perps/community/)
was released on 4 November. It supports separate browser identities, one-use
invitations, immutable plans, independently signed allocations, shared review,
and recovery against persistent SQLite history. Its finite scenario is known in
advance; it does not measure live trading performance.

The Solana oracle-and-pool program is a separate unaudited devnet research track.
Its latest recorded deployment is `devnet-v0.7.0`. New source checks and a hosted
paper beta do not establish that those changes are deployed on-chain.

## 1. An observable, supported first cohort — current phase

Goal: invited communities can complete sessions, understand their own authority
and limitations, and return with useful feedback.

Acceptance criteria:

- Follow a small invited cohort through Plan → My allocation → Review. Record
  consented, minimal feedback on completion, confusion and reasons to return;
  do not publish identities, invite secrets or private room histories.
- Check that participants understand simulated credits, the known scenario,
  per-member approval and browser-key loss before judging retention.
- Exercise lost acknowledgements, restart recovery, invite reuse and revocation
  against the exact release, retaining private evidence and a public summary.
- Establish a verified private reporting route and an assigned human responder;
  publish accurate contact/coverage terms only after they are confirmed.
- Automate encrypted off-host backup refresh, report backup failures to an
  assigned operator, and rehearse restoration on an isolated instance. Preserve
  service identity and reconcile newer client checkpoints; never reset live data
  or erase checkpoints to make an old backup appear current.
- Document a bounded release/incident procedure that preserves the live app,
  Studio, ATSU and the existing community database.

Existing hourly verified on-host backups and an encrypted off-host snapshot are
the starting point. Ongoing off-host replication is not configured yet. The
small beta limits are intentional: 32 rooms, eight lifetime members per room and
64 lifetime invitations per room. Expansion needs capacity and abuse evidence.

## 2. Realistic paper sessions — planned

Goal: communities can test decisions across an uncertain market path with
transparent simulation rules, while all balances remain paper-only.

Acceptance criteria before implementation is called complete:

- Specify admitted instruments, price sources, timestamp/confidence rules,
  stale-feed behavior, session boundaries and outage settlement.
- Make fees, slippage, fills and any liquidation/stop behavior explicit. Label
  the difference between capped finite contracts and perpetual exposure.
- Store enough canonical observations and decisions to reproduce outcomes;
  adversarial tests cover manipulated, missing, reordered and repeated inputs.
- Preserve independent approval, revocation, idempotency and crash recovery as
  sessions become repeatable. Version replay semantics instead of silently
  changing old history.
- Test whether members return for another session and whether reviews help them
  understand decisions. Define measurements before the cohort, without promising
  rewards, profit or product-market fit.

Live prices alone do not create a real exchange, independent oracle assurance or
proof of actual trading performance.

## 3. Economic and operational protocol evidence — separate research track

Goal: resolve the financial model's open obligations before adding real custody.

- Specify liability-aware LP valuation and bounded exit policy. The current
  flat-pool gate contains mispricing but can lock LP exits indefinitely.
- Resolve emergency-price and terminal-data policy, including permanent outages;
  avoid substituting an arbitrary cached mark or rewriting existing claims.
- Model funding farms, reserve grief, insolvency, insurance/bad-debt allocation
  and liquidation incentives with counterexamples and stateful tests.
- Demonstrate pinned-feed refresh and a funded devnet position lifecycle,
  including keeper payout, failed transactions, RPC/oracle outages and restarts.
- Rehearse admin rotation, upgrade controls and legacy-book drain against the
  exact program and ABI intended for deployment.

The [security release gates](docs/security-hardening.md#remaining-release-gates)
contain the current limitations and supporting research.

## 4. Independent assurance and any real-funds decision — gated

No mainnet date or approval is implied. A future decision requires external
review, resolution and retesting of findings, dependency review, sustained
adversarial testing, build/deployment verification, governance and an incident
response process. Product scope, custody and oracle trust must be explicit.

Public security research begins only with a verified private channel, assigned
responder and precise local scope. The [prepared challenge](docs/security-challenge.md)
is not a funded bounty or permission to attack the hosted beta.

## How progress is recorded

Use focused issues and PRs with acceptance criteria and evidence for the exact
commit. Mark source, locally tested, deployed and independently reviewed work
separately. A merged PR closes its implementation task, not every later release
or security gate. Update this roadmap when evidence changes the plan.
