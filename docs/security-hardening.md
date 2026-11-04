# Security hardening

This tracks security implementation through the 2026-11-04 source batch.
These are **source changes, not a deployment record**. The documented devnet
release remains `devnet-v0.7.0`; the new admin instructions require a program
upgrade before clients can call them. The protocol remains unaudited and is not
ready for funds of real value.

## Controls implemented

| Surface | Behavior |
|---|---|
| Dashboard API | RPC URLs and upstream exception text stay on the server. Responses use fixed error codes, and partial failures mark the observation degraded. |
| Dashboard oracle | Missing or invalid data stays unknown. Provenance, verification level, feed identity and cluster-clock freshness are checked. The display is explicitly a comparison against reference defaults, not live-market trading approval. |
| Keeper coverage | A persisted circular cursor visits the whole book across bounded ticks and restarts. Unknown, stale, blocked or unprofitable candidates keep health degraded until a fresh successful observation clears their scope. |
| Keeper submissions | The signed transaction's signature, blockhash, expiry, cost reservation and attempt are saved before broadcast. Ambiguous sends remain pending across restarts. |
| Keeper budgets | Reservations cover measured payer debit, network fees and a cushion. Finalized receipts replace estimates; an overrun stops further sending. Hourly limits and reservations survive restarts and midnight. |
| Exchange admin | The current admin nominates a replacement; only that nominee can accept. The current admin can replace or cancel the nomination. Rotation works while trading is paused. |
| Trading consent | Opens bind an exact risk revision and slot deadline; closes bind a unique position generation. A reused position PDA does not reuse its consent. |
| Borrow history | Every pool equity/reserve mutation checkpoints prior utilization. Markets consume the historical integral; rate changes first accrue at the old rates. Legacy active books must drain before new opens. |
| Funding history | Migrated accounts accrue the full elapsed interval and carry a canonical fractional remainder, including across rate sign changes. |
| Funding lifecycle | Every migrated open/close settles the old interval before sampling the resulting book. A flat book samples zero. Nonempty legacy drain behavior is retained. |
| Emergency prices | Only strictly validated raw oracle observations on active markets can advance the recovery cache. Publication time and provenance are retained; quarantine freezes it. Liquidation-grade observations and legacy entry-price fallbacks cannot supply it. |
| Keeper oracle transport | HTTPS single-feed requests have streamed body/binary caps, a request-and-body deadline, binary/JSON consistency checks, cluster-clock freshness and fixed errors. Receiver verification remains on-chain. |
| RPC and heartbeat transport | Keeper and dashboard HTTP reads have per-attempt deadlines, streamed body limits, caller-abort propagation and redirect refusal. Heartbeat requests have a separate two-second limit. Oversized data fails closed. |
| Signed Risk Rooms | A separate wrapper verifies strict Ed25519 signatures over canonical bounded commands, immutable identity/domain bindings, actor sequences, expiry and current delegation revisions. Paper economics, custody and oracle assumptions remain separate. |
| Rooms browser pilot | The actual Rust engine runs as WASM. Fixed scenario actions and canonical replay enforce the same rules on entry and recovery. Plan metadata binds to genesis. Complete batches commit under strict IndexedDB durability and a transaction-local compare-and-swap before acknowledgement. |
| CI | Keeper/dashboard regressions, TypeScript and dashboard build, plus Rooms browser tests against shipped and fresh WASM, join the Rust, secret-scan, SBF build and LiteSVM checks. |

The admin handshake uses the existing `Exchange.pending_admin` field, preserves
the account layout and appends error codes after the deployed errors. It changes
neither the fee recipient nor the program upgrade authority, and cannot recover
an already-lost admin key.

The [Rooms paper pilot](rooms-paper-pilot.md) is a local browser experiment with
one operator-controlled identity and fixed prices/slots. Its journal is not
replicated and cannot detect a valid older database without an independently
retained checkpoint. Storage clearing can lose the key and records; key loss
leaves verified history read-only and exportable. A non-extractable local key
does not protect against hostile same-origin code. These controls do not supply
custody, live oracle truth, shared-room consensus or a live perpetual market.

## Operating the keeper after upgrading

Keep `DRY_RUN` enabled during initial observation. Invalid boolean or numeric
settings fail at startup. See [keeper configuration](../keeper/README.md) for the
current environment variables and scan-coverage settings.

RPC attempts default to ten seconds and 16 MiB for the complete body, with a JSON
depth cap of 128. Configure `RPC_TIMEOUT_MS` within 1–30,000 and
`RPC_MAX_RESPONSE_BYTES` within 1–67,108,864. HTTP 429 retains web3's bounded retry
schedule, so one RPC operation can take up to five attempts plus 7.5 seconds of
backoff (57.5 seconds at the default timeout). These are per-attempt limits, not
global concurrency control, pagination or WebSocket deadlines. Oversized books
are rejected in full; records are never silently truncated. Successful JSON-RPC
response bytes, including simulation error data, are preserved. Fixed transport
errors do not sanitize arbitrary strings inside a valid provider response.

Preserve `keeper/.state.json` on persistent storage. It is bound to the cluster's
genesis hash, program ID and keeper wallet, and holds spending, scan position and
pending submissions. A process lease prevents two writers using the same journal.
The previous `{day, spentLamports}` ledger is migrated without discarding its
recorded spend; it cannot reconstruct submissions the old process never saved.
Stop and reconcile the old process's outstanding transactions before switching.

Do not delete the journal to clear a warning. Unreadable or mismatched state fails
closed. A missing finalized receipt retains its reservation. Only after finalized
block height passes transaction expiry and repeated history checks find nothing
can an unobserved submission be retired; its full reservation is charged because
missing archival history does not prove zero cost.

Cost estimation requires payer balance and simulation results from the same
confirmed slot. If that cannot be obtained, the keeper refuses the send and
reports degraded health. This can reduce throughput on some RPC providers.
Reservations are conservative estimates, not an on-chain spending cap: state or
program changes between simulation and execution can increase cost. Finalized
cost reconciliation detects an underestimate and latches a stop. Use one keeper
process and a dedicated wallet, and investigate before clearing any stop.

## Admin transfer procedure

After the tested program artifact is upgraded and its generated IDL is installed:

1. The current admin signs `propose_admin(new_admin)` for the canonical exchange.
2. Read the exchange back and verify `admin` is unchanged and `pending_admin`
   matches the intended signer.
3. The nominated signer signs `accept_admin()` for that exchange.
4. Read back the new `admin` and a cleared `pending_admin`.

Before acceptance, the current admin can sign `cancel_admin_transfer()` or issue
a replacement nomination. A nomination grants no admin permissions. Acceptance
revokes the old admin immediately. A multisig PDA can accept via its own signed
CPI, but production multisig/timelock integration still needs separate testing.

## Verification

Offline checks require no wallet, RPC credentials or live transactions:

```sh
npm ci --ignore-scripts
npx tsc --noEmit
npm run test:keeper
npm --prefix dashboard ci --ignore-scripts
npm --prefix dashboard test
npm --prefix dashboard run build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p sakura-perps-risk --locked
cargo test -p sakura-perps --lib --locked
scripts/security-challenge.sh paper
```

The [browser package](../integrations/sakura-rooms/README.md) documents its
separate WASM build, pinned bindings tool and Chromium prerequisites. Its tests
run the shipped engine and a fresh build, exercising transaction aborts,
concurrent-tab writes, key loss, metadata binding and bounded replay.

The [security challenge](security-challenge.md) provides one local entry point
and a source/build manifest for reproductions. CI additionally builds the SBF
program with the pinned Agave/Anchor versions and
runs the full LiteSVM suite against that fresh artifact. Admin regressions cover
authorization, signatures, cancellation/replacement, pause behavior, canonical
exchange constraints, unchanged fields and rejected-call rollback. Regenerate
the committed JSON IDL and TypeScript type from the same build when instructions
change. A host test alone does not verify the deployed runtime.

## Remaining release gates

1. **LP valuation and exit fairness.** The 2026-11-02 source blocks deposits and
   redemptions until both locked collateral and profit reserves are zero.
   This contains known mispricing paths, but positions can block LP exit
   indefinitely. The 20% reserve ceiling does not bound omitted trader losses.
   Specify and test liability-aware valuation and bounded exits. Historical
   utilization now prevents repricing elapsed borrowing on migrated books;
   existing legacy exposure must drain on its old accounting first. See the
   [Percolator and Risk Rooms report](research/2026-11-02-percolator-and-risk-rooms.md).
2. **Emergency prices and oracle policy.** Strict snapshot provenance now
   excludes liquidation-grade updates and freezes the cache in quarantine.
   Review how stale historical settlement distributes value and the relationship
   between margin, confidence, EMA clamping and spread. A legacy unproven cache
   plus a permanent oracle outage can still lock positions.
3. **Economic and insolvency behavior.** Model funding farms, reserve grief,
   liquidation incentives under load, insurance and bad-debt recovery. New
   accounting removes settlement-window truncation; the drain-only legacy path
   preserves it. Existing invariant tests do not settle the remaining policies.
4. **Operational proof.** Exercise the keeper against a funded test book through
   RPC outages, oracle failure, restart, rate limits and failed transactions.
   An empty-book preflight does not prove liquidation execution. Rehearse admin
   rotation and program upgrades, including multisig and timelock controls.
5. **Dependency remediation.** The Hermes/Zod network parsing path was replaced
   and Mocha's vulnerable serializer patched. As checked on 2026-11-02, root
   `npm audit` reports 7 high / 11 moderate; the production-only root graph has
   7 high / 8 moderate; dashboard has 2 high / 5 moderate. On 2026-11-03, moving
   SPL Token to development-only fixture dependencies and using verified ATA PDA
   derivation removes `bigint-buffer` from the keeper's production graph. The
   production-only root audit now reports 4 high / 6 moderate. These are affected
   package entries, including inherited effects, not unique exploit counts.
   Remaining paths are analyzed below. Re-run audit when evaluating a release;
   replacements or breaking upgrades need compatibility testing, not forced
   lockfile overrides.
6. **Independent assurance.** Add long-running adversarial/stateful fuzzing,
   reproducible-build verification, external review and an incident runbook.
   Resolve findings and verify the exact deployment before considering mainnet.

The README's historical deployment notes and this source-hardening work should
not be read as a security certification. Report vulnerabilities through
[SECURITY.md](../SECURITY.md).

## Dependency findings and present call paths

This analysis is bounded to the checked-in keeper/dashboard call paths. It does
not suppress scanner results, prove every transitive call safe, or cover future
uses of these packages.

| Remaining family | Current use and limit of the assessment |
|---|---|
| Anchor → `toml@3.0.0` (high) | Anchor's workspace loader parses the local `Anchor.toml`. The keeper/dashboard instantiate `Program` from tracked IDL and do not use `anchor.workspace`; the initialization test does. No remote TOML-input path was found. The current Anchor range cannot adopt TOML 5 as a compatible patch. |
| SPL Token → `bigint-buffer@1.1.5` (high, now development-only) | Network fixture scripts retain SPL Token. Production keeper ATA derivation uses the official owner/token-program/mint PDA seeds through web3, independently checked against the SDK for both token programs. `npm ls --omit=dev` has neither SPL Token nor bigint-buffer. The upstream advisory is not patched in the development graph. |
| web3/jayson → `stream-json@1.9.1` (moderate) | web3 uses Jayson's browser HTTP client, not the affected server streaming/filter/assembler interfaces. No affected streaming path was found. |
| jayson → `uuid@8.3.2` (moderate) | The observed caller uses `v4()` without a caller-supplied buffer; the advisory concerns other version functions and buffer handling. |
| Mocha → glob/minimatch (moderate, development) | Test patterns come from checked-in commands. Mocha's current dependency ranges do not accept the patched major versions. These packages are absent from the production dependency audit. |

Relevant primary advisories include
[TOML](https://github.com/advisories/GHSA-v5mp-jgw5-2x6j),
[bigint-buffer](https://github.com/advisories/GHSA-3gc7-fjrx-p6mg),
[stream-json](https://github.com/advisories/GHSA-528h-pc64-c93x),
[uuid](https://github.com/advisories/GHSA-w5hq-g745-h8pq),
[glob](https://github.com/advisories/GHSA-3g3m-2fjr-6pw4) and
[minimatch](https://github.com/advisories/GHSA-g3hv-75wg-m249).
The removed Hermes dependency used the affected
[Zod parsing family](https://github.com/advisories/GHSA-mrhx-c7xc-m3w5).
Mocha 12 resolves a patched serializer for
[serialize-javascript](https://github.com/advisories/GHSA-5c6j-r48x-rmvq).

The replacement Hermes reader caps decompressed streamed JSON at 64 KiB and a
single binary update at 16 KiB, rejects redirects, and limits the full request
and body to ten seconds. It checks exact feed and price-message consistency,
using cluster time advanced by monotonic elapsed time. This is input validation;
VAA signatures and Merkle proofs still require Pyth's receiver, followed by
Sakura's on-chain oracle guards. A structurally valid payload is not a verified
price and cannot authorize a trade by itself.
