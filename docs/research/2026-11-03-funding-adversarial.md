# Funding lifecycle review — 3 November 2026

Baseline: `b5456c9`, reviewed locally without a wallet, live transaction or deployment. This note records a defensive accounting change and its scope. It does not certify the perpetuals economics or propose a forced liquidation policy.

## Finding and selected implementation

The funding sample was updated before an exposure mutation, subject to `min_settle_interval_seconds`, but neither shared position ledger updated it after the mutation. Consequently the sample could describe a different book: an empty market could retain a nonzero sample, and a newly opened or partially closed book could retain the prior exposure's rate until another sampling call. Accruing at the old sample before deriving a new one then attributed the whole intervening period to that outdated book.

The normal lifecycle regression `funding_samples_follow_normal_long_flat_short_lifecycle` fails against the baseline SBF binary on the first successful open: a long-only market retains a zero sample rather than the configured positive rate. The same invariant requires an empty book to sample zero and a short-only book to sample the configured negative rate.

The implementation now treats each migrated exposure mutation as a funding interval boundary:

1. Accrue the old book through the existing clock high-water boundary.
2. Apply the open or close ledger.
3. Compute the future funding sample from the resulting long and short entry notional.
4. Preserve all accrued indices and fractional carries. Sampling does not retroactively rewrite them.

The sample is updated in the shared ledger functions, covering open, ordinary close, admin settlement, emergency close and keeper liquidation. The helper keeps the sampling timestamp monotonic. A later transaction failure rolls back both the accrued interval and the new sample through normal Solana transaction atomicity.

This changes the future rate immediately after an exposure change. That is the intended lifecycle behavior; it does not promise a trader a fixed funding rate or change their snapshotted price, fee, margin or profit-cap terms. Account sizes, instruction arguments, errors, oracle rules and LP valuation gates are unchanged.

## Cadence and legacy behavior

For migrated books, `min_settle_interval_seconds` no longer delays economically relevant sampling. Exposure changes and risk retunes both sample immediately after settling the prior interval. A standalone settle can still use the field to throttle redundant sampling while the exposure and risk parameters are unchanged. The field remains in the ABI and continues to govern the original sampling cadence of nonempty legacy drain-only books.

Nonempty legacy books retain their old drain accounting, including their sampling cadence, elapsed-time cap and stateless funding rounding. The final legacy close explicitly resets the empty book's sample to zero. A subsequent empty-book migration and new open cannot inherit an old nonzero sample. This does not reconstruct missing historical utilization, forgive already recorded indices or make the legacy drain time-bounded.

## Acceptance tests

The focused additions are:

- Normal long → flat → short → flat transitions, with exact signed indices, carried fractions and sampling timestamps. The first interval's debt remains charged at its old rate; opening the opposite side does not rewrite it.
- A balanced book, added directional exposure and partial closes. Every resulting sample matches the remaining book, and a same-clock close changes only the future sample.
- A backwards clock revision during close and reopen. Neither settlement nor sampling timestamps rewind, and no already charged time is accrued twice.
- A late token-transfer failure after the close ledger. A harness-only frozen token account forces the failure; all market, pool, position and token state remains unchanged. This is failure injection, not an assumed attacker ability to freeze the configured mint.
- A host check that resampling preserves accrued indices/carry, keeps clock high-water marks and retains nonempty legacy sampling until the book becomes flat.

The baseline failure was reproduced before the implementation. Program host tests pass 36/36. All 69 positions tests pass against the freshly rebuilt SBF binary, including the four new lifecycle regressions and existing migration, intent, LP, oracle and compute-budget cases. The complete challenge run remains the integrated release check for the other account-layout, risk, custody and service targets.

## What this does not establish

Funding credits remain part of each position's pool-backed capped payoff. This lifecycle correction does not prove that every credited amount has already been collected from another position. Capped losses, unsettled funding payers, liquidation incentives and bad-debt attribution still need a separate economic policy and state-machine analysis. Conservation and maximum-payout reservation bound custody obligations; they do not by themselves prove fair funding transfers or absence of all profitable strategies.

LP deposits and redemptions still require a flat pool. A guaranteed fair exit deadline cannot be added merely by accepting a lower share quote or forcing settlement against whatever cached price is available. It requires an explicit contract policy for the terminal mark, publication/outage rules, funding cutoff, complete liability inventory and allocation among claimants. Existing perpetual claims cannot silently be rewritten into finite contracts.

After this narrow accounting correction, authenticated canonical Risk Rooms commands are a useful independent next milestone. They can bind owner/delegate authority, domain, nonce, expiry and policy state without pretending that the current paper model's asserted identities are authenticated. That work does not resolve oracle truth, custody, durable concurrency, terminal-data outages or the perpetual pool's LP exit policy. Bounded exit should progress as a separate specified successor-contract model with order-independent settlement tests, not as an unreviewed emergency haircut in the current pool.
