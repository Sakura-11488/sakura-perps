# Sakura Perps security challenge

**Launch status: prepared for local testing; public announcement is pending a
verified private reporting channel and an assigned responder.**

This challenge targets an **unaudited local research build**, using
simulated funds. It is not a mainnet launch, a funded bug bounty, or a claim that
the system is secure. The existing shared devnet deployment is older than this
source and is **not the challenge target**.

The challenge includes the Solana perpetuals program, its keeper and read-only
dashboard, and a separate Risk Rooms paper model for community strategies.
Risk Rooms uses finite capped contracts; it does not implement a live perp.
Its [signed-command wrapper](../crates/rooms-auth/README.md) now verifies real
Ed25519 authorizations. The underlying paper kernel remains available separately
for economic experiments, with its original asserted-identity assumptions.

## Start without a wallet

Install Rust 1.89.0 through rustup, clone the repository, and record your commit:

```sh
git clone https://github.com/Sakura-11488/sakura-perps.git
cd sakura-perps
git rev-parse HEAD
scripts/security-challenge.sh paper
```

The paper command runs the room and signed-wrapper tests, a community season,
and the signed-room demonstration. It
needs no Solana installation, wallet, RPC account or environment file. Package
downloads may need network access. The example asserts rejected attacks as well
as successful trades; it fails if the expected protections stop working.

For the compiled Solana program, install the [supported toolchain](../README.md#toolchain):
host Rust 1.89.0, Agave 4.1.2, SBF platform-tools v1.54, Anchor CLI 1.1.2 and
Node 22 with npm. Put rustup's **cargo and rustc** first in `PATH`. Linux may
also require `pkg-config`, `libssl-dev` and `libudev-dev`.

```sh
scripts/security-challenge.sh full
```

Full mode runs formatting and clippy, risk and program host tests, builds the
SBF binary and IDL with locked dependencies, checks the committed ABI, executes
the LiteSVM transactions against that binary, and tests the keeper and dashboard.
It does not start the keeper, submit transactions or read a wallet. Avoid
`anchor test` and `tests/devnet-*.ts` for this challenge: those are separate
network-oriented tools.

After success, `target/security-challenge/manifest.json` records the Git commit,
whether the checkout was dirty, tool versions, source hashes, and hashes of the
compiled program and generated ABI. Include it with a report. A binary hash
identifies that build; it is not a claim that all platforms produce identical
bytes. CI publishes the same manifest with its `program` artifact. Check that
**all six CI jobs passed**, including the SVM job which runs after the build.
The manifest alone is not a test or deployment attestation.

Local validation on 2026-11-02 passed **297 tests**: 107 compiled-program SVM,
98 risk, 35 program host, 17 Rooms, 26 keeper and 14 dashboard tests. The community
scenario, strict clippy, formatting, TypeScript and generated-ABI comparison
also passed. The dashboard production build passed separately. These finite
checks are regression evidence, not an independent audit or exhaustive search.

The 2026-11-03 source batch passed **340 tests** through the full entry point:
111 SVM, 98 risk, 36 program host, 17 kernel Rooms, 20 authenticated Rooms,
40 keeper and 18 dashboard tests. Both examples, fresh SBF/IDL generation,
strict clippy, formatting and TypeScript passed; the dashboard production build
passed separately. This adds evidence for funding lifecycle, signed commands
and bounded network operations without changing the launch/intake status above.

## Where to attack

| Target | Useful counterexample |
|---|---|
| `programs/sakura-perps/src/position.rs` | A stale policy, expired intent, reused position generation, substituted account or unauthorized signer moves funds. |
| `pool.rs` and `market.rs` | A sequence of markets, LP operations, accrual calls or rate changes reprices already elapsed borrowing, loses debt, breaks reserves or changes payouts through settlement cadence. |
| `oracle.rs` and emergency exits | A wrong feed, unverified/future/stale update or liquidation-grade observation becomes a strict recovery snapshot; quarantine permits the snapshot to move. |
| `crates/risk` | Checked integer arithmetic, rounding, funding carry or settlement fails at a boundary or under an equivalent partition of time. |
| `crates/rooms` | A delegate exceeds an owner's cumulative budget, spends the same remaining limit twice, accesses another backing cell, changes accepted economics or settles to a different beneficiary. |
| `crates/rooms-auth` | A wrong key/domain, replay, expired authorization, altered canonical bytes or revoked-and-regranted delegation bypasses signature checks; a failed command consumes a sequence or modifies state; replay accepts a changed transcript against the original trusted head. |
| `keeper/src` | Restart, ambiguous broadcast, receipt loss, bad oracle responses or RPC errors bypass the spending reservation, duplicate a send, conceal an unhealthy book or expose credentials. |
| `dashboard/lib` and `dashboard/app` | Server credentials escape, invalid observations appear healthy, or an upstream failure becomes trusted market information. |

The pool's historical utilization accumulator is checkpointed **before** each
change to available liquidity or reserves. Each migrated market consumes only
the integral since its own checkpoint. Risk retuning accrues at the old rates
before replacing them. Migrated accrual retains the full elapsed interval;
funding retains a nonnegative sub-index remainder, including across rate sign
changes. These are specific testable claims, not an assertion that the economic
design is complete.

Migrated exposure changes now close the old funding interval and sample the
resulting book prospectively, including zero for an empty book. The former
sampling cadence cannot leave a previous book's rate attached to new exposure.
See the [funding lifecycle regression](research/2026-11-03-funding-adversarial.md).

Recovery prices are raw observations accepted under strict trading checks,
recorded at the oracle's publication time. Only newer observations on an active
market can update the cache. Liquidation checks cannot populate it. Quarantine
freezes it, and emergency close requires its provenance marker. Missing or
legacy unproven snapshots fail closed instead of substituting entry price.

Useful focused commands after a full build:

```sh
cargo test -p sakura-perps-risk --locked
cargo test -p sakura-perps-rooms --locked
cargo test -p sakura-perps-rooms-auth --locked
SAKURA_PERPS_SO="$PWD/target/deploy/sakura_perps.so" cargo test -p sakura-perps-svm-tests --locked
npm run test:keeper
npm --prefix dashboard test
```

Add a failing regression beside the closest existing test. LiteSVM fixtures can
mint simulated collateral, advance the clock and insert oracle accounts. Those
are test-harness privileges: a report must explain which actions a real attacker
could perform and which depend on a trusted administrator, oracle or validator.

## Threat model and known limitations

Assume traders, LPs, delegated strategies and keepers can be malicious. Test
reordering, retries, competing markets, boundary arithmetic, late data and
failure after partial calculation. Rejected operations should preserve value
and authority. A test that requires editing arbitrary program-owned accounts
does not by itself establish a permissionless attack.

The following are unresolved and are not hidden behind a passing suite:

- **LP exits can be blocked indefinitely.** Deposits and redemptions require a
  flat pool with no locked collateral or profit reserves. An open position can
  prevent that. Liability-aware valuation and bounded exits remain design gates.
- **Legacy accounting needs a drain.** Reserved account bytes decode without
  resizing, but past utilization cannot be reconstructed. An unmigrated pool
  with existing exposure rejects new opens; existing positions drain using the
  old clamped accounting until it is flat. This does not repair historical fees
  or give the legacy drain the new engine's temporal guarantees. Coordinate the
  program and generated-client upgrade; `set_risk_params` now requires the
  canonical writable pool, as does `settle_market`.
- **Emergency settlement trusts historical price policy and admin authority.**
  A proven cached observation can be economically stale. It is not guaranteed
  to be a fair current exit price. An unproven legacy snapshot plus a permanent
  oracle outage can leave positions locked. Normal EMA-clamped pricing also
  needs economic review under rapid market moves.
- **Funding, liquidation and insolvency need further research.** Funding farms,
  reserve grief, incentives under congestion, insurance and bad-debt recovery
  are not solved by temporal accounting fixes.
- **Risk Rooms is an in-memory model.** Direct kernel identities are asserted.
  The signed wrapper authenticates commands against an immutable key registry;
  it does not verify collateral transfers, oracle truth or operator time. Its
  hash chain needs an independently trusted genesis/head checkpoint and complete
  replay after restart. There is no durable storage, key recovery or consensus.
  Its administrator supplies immutable
  terminal observations. Missing terminal data locks reserves; there is no
  guaranteed permanent-outage exit. The raw kernel transcript is not a signed
  ledger; wrapper evidence still has the separate trust limits above.
- **Governance and operations remain trusted.** Admin feed qualification,
  mutable risk policy, upgrade authority, RPC availability and keeper operation
  need deployment rehearsal and independent review. Local checks do not prove
  sustained keeper execution or the bytecode of a deployed address.
- **Dependency advisories remain.** See [hardening notes](security-hardening.md).
  Bounded oracle parsing and patched test tooling remove specific paths; they
  do not turn the remaining dependency graph into an audited one.

See the [Risk Rooms specification](../crates/rooms/README.md) and
[Percolator research](research/2026-11-02-percolator-and-risk-rooms.md) for the
reasoning and rejected alternatives. Reports showing a new consequence of a
known limitation are welcome; explain what changes the assessment.

## Scope and private reports

Run experiments on your own machine with simulated assets. This invitation does
not authorize attacks on the live Sakura app, shared devnet deployment, public
RPCs, oracle services, other users or other projects. There is no funded bounty
or reward promise. The [security policy](../SECURITY.md) gives the reporting and
good-faith research terms.

**Private intake is not configured.** The previous email address was unverified
and has been removed. GitHub private vulnerability reporting is disabled;
enabling it requires a repository administrator. The public invitation must wait
until the owner enables that route and assigns a responder, or publishes another
verified private channel in [SECURITY.md](../SECURITY.md). Do not send sensitive
findings to an unverified address or publish them in a public issue/PR. A public
request for a security contact should contain no exploit details.

Prepare the following for the verified private channel when it is available:

1. Commit, manifest and whether you changed the source or harness.
2. Minimal failing test, exact command, expected result and observed result.
3. Initial/final balances and the attacker gain, victim loss or lockup.
4. Required roles, keys, privileges, oracle assumptions and timing.
5. Whether the issue affects fresh state, legacy migration, the paper model or
   a service; include the smallest proposed correction if available.

Do not attach private keys, credentials or other people's data.

## Suggested announcement

Draft only: publish after private intake and responder coverage are confirmed.

> Sakura Perps is opening its local security research challenge. Try to break
> the accounting, trade consent, oracle recovery, keeper safety or our community
> Risk Rooms paper model. We provide a compiled-program simulator, regression
> tests and source/build manifests. Use simulated funds on your own machine.
> This is unaudited research, not a mainnet launch or funded bounty. Known
> limitations and private reporting instructions are in the challenge guide:
> https://github.com/Sakura-11488/sakura-perps/blob/main/docs/security-challenge.md

To use GitHub intake, a repository administrator can enable **Private
vulnerability reporting** under the repository's **Settings → Advanced Security**.
Then verify that **Report a vulnerability** appears under **Security →
Advisories**, assign a responder and update this launch status.
[GitHub's setup instructions](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/configure-vulnerability-reporting/configure-for-a-repository)
describe the required permissions and notification settings. The current
connection has push access but not admin access, so it cannot enable this setting.
