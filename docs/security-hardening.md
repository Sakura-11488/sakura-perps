# Security hardening

This is the first security implementation batch after the read-only dashboard.
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
| CI | Offline keeper and dashboard regressions, TypeScript checking and the dashboard production build join the existing Rust, secret-scan, SBF build and LiteSVM checks. |

The admin handshake uses the existing `Exchange.pending_admin` field, preserves
the account layout and appends error codes after the deployed errors. It changes
neither the fee recipient nor the program upgrade authority, and cannot recover
an already-lost admin key.

## Operating the keeper after upgrading

Keep `DRY_RUN` enabled during initial observation. Invalid boolean or numeric
settings fail at startup. See [keeper configuration](../keeper/README.md) for the
current environment variables and scan-coverage settings.

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
```

CI additionally builds the SBF program with the pinned Agave/Anchor versions and
runs the full LiteSVM suite against that fresh artifact. Admin regressions cover
authorization, signatures, cancellation/replacement, pause behavior, canonical
exchange constraints, unchanged fields and rejected-call rollback. Regenerate
the committed JSON IDL and TypeScript type from the same build when instructions
change. A host test alone does not verify the deployed runtime.

## Remaining release gates

1. **LP valuation and exit fairness.** Share pricing still uses
   `pool.quote_deposited` without open-position liabilities. The 20% reserve
   utilization ceiling bounds exposure; it is not a valuation fix. Specify and
   test liability-aware deposits/withdrawals before increasing limits.
2. **Emergency prices and oracle policy.** Review how liquidation-grade updates
   can refresh `last_good_price`, how stale emergency settlement distributes
   value, and the relationship between margin, confidence and spread. Add
   adversarial scenarios for outages, revocation and recovery.
3. **Economic and insolvency behavior.** Model funding farms, reserve grief,
   settlement-window truncation, liquidation incentives under load, insurance
   and bad-debt recovery. Existing invariant tests do not settle these policies.
4. **Operational proof.** Exercise the keeper against a funded test book through
   RPC outages, oracle failure, restart, rate limits and failed transactions.
   An empty-book preflight does not prove liquidation execution. Rehearse admin
   rotation and program upgrades, including multisig and timelock controls.
5. **Dependency remediation.** Compatible dependency patches were applied, but
   `npm audit` still reports advisories. At this change, root dependencies report
   11 high / 9 moderate; dashboard dependencies report 2 high / 5 moderate.
   Remaining paths include Anchor/TOML, web3/jayson, Pyth/Zod, bigint-buffer and
   development tooling. Re-run audit when evaluating a release; replacements or
   breaking upgrades need compatibility testing, not forced lockfile overrides.
6. **Independent assurance.** Add long-running adversarial/stateful fuzzing,
   reproducible-build verification, external review and an incident runbook.
   Resolve findings and verify the exact deployment before considering mainnet.

The README's historical deployment notes and this source-hardening work should
not be read as a security certification. Report vulnerabilities through
[SECURITY.md](../SECURITY.md).
