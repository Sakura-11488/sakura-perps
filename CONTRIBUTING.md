# Contributing to Sakura Perps

Start with the [repository map](README.md#repository-map), [roadmap](ROADMAP.md)
and the component's README. The immediate product phase is the invited paper
cohort. The Solana protocol has separate economic and operational release gates.

## Scope a change

Use a focused issue or PR describing the problem, proposed behavior and observable
acceptance criteria. For a defect, include a minimal local reproduction and the
commit/environment tested. Explain any changes to authority, accounting, replay,
storage or external trust. Keep unrelated changes in separate PRs.

Sensitive vulnerabilities belong under [SECURITY.md](SECURITY.md), not ordinary
issues. Do not post exploit details while the private reporting route is pending.
Do not upload credentials, invitation fragments, browser keys, database files or
private room exports. Use generated fixtures and redact diagnostic output.

Use a feature branch and open a PR against `main`. Existing project work uses
`codex/<description>`; other descriptive contributor branch names are welcome.
Do not rewrite shared history or fold private application code into this repository.

## Source ownership and compatibility

- `community/protocol.mjs` is the shared wire-verifier source. Its browser copies
  must match; use the packaging commands in the [integration guide](integrations/sakura-rooms/README.md).
- The Rust engine owns paper accounting. Do not add a second JavaScript ledger
  that can diverge from replayed signed records.
- `integrations/sakura-rooms` contains only the allowlisted public Expo package.
  Preserve web/native variants and keep private Studio source out of public Git.
- Version protocol or replay-semantic changes deliberately. A schema rename is
  not permission to reinterpret existing signed history or reset persisted state.
- Program instruction changes require regenerated IDL/types and matching tests.
  Do not describe source changes as deployed without exact release evidence.
- Keep old hashed browser engine assets available during a website release so
  existing tabs can finish recovery. Packaging and deployment are separate steps.

## Validation

Use the [pinned toolchain](README.md#toolchain). All commands below run from the
repository root unless a `--prefix` is supplied. Install locked dependencies with
`npm ci --ignore-scripts`; do not force dependency overrides to hide findings.

Choose checks for the changed behavior, then require all configured CI checks
before merge. Documentation-only changes need link/format review; do not invent
runtime evidence or tests that only repeat the implementation.

| Changed area | Relevant commands |
|---|---|
| Rust formatting and types | `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings` |
| Risk mathematics | `cargo test -p sakura-perps-risk --locked` |
| Rooms kernels and authorization | `scripts/security-challenge.sh paper` |
| Browser/community integration | `node scripts/package-rooms.mjs check`; `npm --prefix integrations/sakura-rooms run typecheck`; `npm --prefix integrations/sakura-rooms test` |
| Keeper and clients | `npm exec -- tsc --noEmit`; `npm run test:keeper` |
| Dashboard | `npm --prefix dashboard test`; `npm --prefix dashboard run build` |
| Program, ABI and integrated security regression | `scripts/security-challenge.sh full`, after its documented prerequisites |

Install browser-test dependencies and Chromium before the browser suite:

```sh
npm --prefix integrations/sakura-rooms ci --ignore-scripts
(cd integrations/sakura-rooms && npm exec -- playwright install chromium)
```

Linux browser system libraries and fresh WASM build requirements are documented
in the [integration guide](integrations/sakura-rooms/README.md#rebuild-and-verify).
Use the [full challenge prerequisites](docs/security-challenge.md#start-without-a-wallet)
for SBF, IDL and compiled-program verification. The dashboard build is a separate
check; the full script tests the dashboard but does not build it.

The full CI workflow builds SBF once and runs SVM tests against that artifact.
Rooms checks run against both checked-in and freshly built WASM. A missing
artifact, skipped test or partial check is not a passing release.

`anchor test` and `tests/devnet-*.ts` can interact with a network or wallet. They
are outside the wallet-free local challenge; do not substitute them for the
documented commands or run them merely to verify documentation.

## Application and hosting boundary

The full website is built from the separate Sakura Expo source and verified at
its `/app/` base path. The integration component harness does not establish that
the complete application or native UI works. Follow the exported-app tests in
the integration guide when changing that experience.

Website publication follows the owner's private Studio web release process.
Preserve Studio, the required ATSU hosting restoration, community proxy handlers,
persistent service identity/database and older hashed assets. A public branch
export alone must not replace the live app. Never commit production secrets or
copy private Studio source to a public remote.

## Review and evidence

A PR should identify what changed, why, the tests actually run, known limitations
and any compatibility/release steps. Reference the exact commit or generated
manifest for build evidence. Distinguish implemented, tested and deployed states.
Retain attribution and dated source references in research notes.

The repository is [BUSL-1.1 source-available](LICENSE). Keep existing license and
copyright notices; do not present this work as audited, independently certified
or unrestricted open-source software.
