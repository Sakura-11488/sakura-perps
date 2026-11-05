# Sakura Rooms — browser paper pilot

The bundle also includes the [invite-only community beta](../../docs/community-rooms-beta.md)
at `/app/perps/community`. It uses the same Rust engine plus the dedicated
[`community` service](../../community/README.md). Local-only Rooms remains at
`/app/perps/rooms`. Community requires a same-origin `/api/rooms/v1/` backend with
persistent storage; a static export alone cannot provide shared history.

Plan → My allocation → Review, styled for Sakura's existing pink reading room.
This is a **single-browser paper pilot**: no wallet connection, deposits, live
orders, synchronized members, or shared pool. The fixed SOL scenario ends at
104 after opening at 100. The pictured 92 midpoint is illustrative; it does not
execute a stop, liquidation, or intermediate fill. Known prices make this a
workflow and limits exercise, not evidence of trading skill.

The public integration lives here because Sakura's current Expo checkout has
unrelated work and private Studio history. Only explicitly allowlisted Rooms
sources, the existing public roadmap stylesheet, and the full-app browser test
are captured. The production app and its private dependencies are not vendored.

## What each screen does

- **Plan:** write a title, thesis and invalidation condition. Publishing binds
  them to a new signed history. An invalidation note is not an automatic order.
- **My allocation:** choose 1–100 whole paper credits, long/short, and 1–3×
  leverage. Review collateral, fee, remainder and capped payout, then explicitly
  approve. One allocation is allowed per plan; input changes clear approval.
- **Review:** run the fixed scenario once, view the actual Rust-calculated net
  result after fees, reopen local history, export public records, or verify an
  exported record in a read-only view.

## Trust and recovery boundaries

The same Rust `rooms` / `rooms-auth` accounting and canonical Ed25519 intents
run through `crates/rooms-web` in WASM. JavaScript never calculates settlement.
The local browser operator controls the paper member, backer and administrator;
this is not independent authentication of a remote leader or real oracle.

A nonextractable Web Crypto Ed25519 private key is stored with its public record
in IndexedDB. It is not a trading wallet. Public exports contain no private key.
Nonextractability prevents the standard export operation; it does **not** stop
malicious same-origin code from using the key. XSS, compromised hosting, an
untrusted browser, extensions, and a malicious local operator remain outside
the pilot's trust boundary.

Signing and replay build a candidate state in disposable WASM memory. A strict
IndexedDB transaction saves a whole action batch and the identity together.
The UI acknowledges only `transaction.oncomplete`, never a request's success:

| Accepted phase | Signed/replayed records | Atomic save |
| --- | ---: | --- |
| Published | 4 | Setup + paper identity + plan binding |
| Approved | 6 | Contribution + allocation |
| Complete | 8 | Terminal observation + settlement |

An aborted write retains the prior phase. A committed write with a lost UI
acknowledgement is recovered by replaying saved history, without resubmission.
Two tabs compete using an atomic compare-and-swap of the previous public record;
only one advances it. The losing tab must reload and review the current state.
Unknown schema/engine versions and corrupt records fail closed without deleting
them. A missing or mismatched key leaves a verified read-only history that can
still be exported. No replacement key is silently created for that room.

These are **crash-consistent browser saves**, not a backup or a hardware guarantee.
Clearing/evicting site data, losing the device, or storage/OS failure can destroy
the journal and identity. Read-only exports preserve verifiable public history
but cannot restore signing capability. Strict durability support is required;
unsupported browsers refuse writes. Chromium is tested, not every browser.

An exported head authenticates that exact history, **not its freshness**. A valid
older prefix, a rollback of the entire local database, or equivocation by its
operator requires an independently retained trusted checkpoint to detect.
This pilot has no remote append-only witness, independent oracle, or multi-device
anti-rollback service. Dates are local browser time. Never treat an export as
proof of a live account balance or trading performance.

## Install into the current Sakura Expo source

The destination must have `sakura-mobile/app/_layout.tsx` and the existing Sakura
theme at `sakura-mobile/lib/theme`. Web and native route variants are preserved;
native shows an availability explanation. This integration adds no app dependency.

From this repository:

```sh
node scripts/package-rooms.mjs check
node scripts/package-rooms.mjs install /absolute/path/to/Sakura
```

The installer preflights all sources and refuses to overwrite different existing
files. Merge an existing integration deliberately, then use `install-engine` to
copy only the generated engine and version module. Do not replace the app shell,
theme, Studio, or hosting checkout. Add this link alongside existing actions in
`sakura-mobile/app/perps.web.tsx` using its existing `Link` import:

```tsx
<Link href="/perps/rooms" asChild>
  <a className="perps-text-link">Try Sakura Rooms · paper pilot</a>
</Link>
```

Run root `npm run build` in Sakura and test **`/app/perps/rooms`** from the full
`web/dist` export. The app's normal static server must map `/app/` to that export,
including `/app/rooms-engine/*`. Engine assets are same-origin, content-addressed
and pinned with JavaScript SRI plus a SHA-256 check before WASM initialization.
Each fresh load attempt creates a new pinned engine, including after an outage.
Keep previously deployed hashed assets available while older app tabs remain open.

Installing this package changes source only. The community paper beta was
released separately on 4 November 2026; see its [hosted release status](../../docs/community-rooms-beta.md#hosted-release-and-next-operational-gates).
Sakura's required private Studio release workflow and ATSU hosting restoration
still apply; an export of a public branch alone must not replace the production
app or omit the community service and proxy handlers.

## Rebuild and verify

Use Rust 1.89.0 and Node 22. The CLI version must match the locked wasm-bindgen
library version:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm --prefix integrations/sakura-rooms ci --ignore-scripts
bash scripts/build-rooms-web.sh
node scripts/package-rooms.mjs check
npm --prefix integrations/sakura-rooms run typecheck
cd integrations/sakura-rooms
npx playwright install chromium
npm test
```

On a Linux host missing browser system libraries, run
`npm exec -- playwright install --with-deps chromium` from the integration
directory. Installing those OS packages may require administrative access.

`npm test` bundles the exact shipped client and component, then uses real
Chromium, Web Crypto, IndexedDB and the actual WASM. Recovery tests inject storage
aborts, competing tabs, lost acknowledgements and malformed histories. The
component test covers approval resets, exact amounts, reload, export, read-only
verification and literal rendering of adversarial plan text. Test adapters stand
in for Expo navigation and theme **only**; no accounting or storage is mocked.
`typecheck` covers browser sources using those adapters. It does not claim to
typecheck the entire Sakura app or the native fallback.

For the real exported application, after root `npm run build` in Sakura:

```sh
SAKURA_BROWSER_TEST_NODE_MODULES=/absolute/path/to/sakura-perps/integrations/sakura-rooms/node_modules \
  node --test web/test/rooms-app.browser.mjs
```

That test runs the complete Expo export at `/app/`, desktop/mobile and light/dark,
including navigation, keyboard approval, exact long/short outcomes, reload,
exports and engine outage recovery. It blocks external network traffic.

`node scripts/package-rooms.mjs capture /absolute/path/to/Sakura` refreshes only
the source allowlist after reviewed app edits. `check /absolute/path/to/Sakura`
compares it with the authored files. Never capture an entire application tree.
The semantic format is `sakura-rooms-paper/1`; do not change replay behavior under
the same semantic version. A future format needs explicit migration or read-only
legacy recovery. Content hashes identify the packaged bytes; a matching hash
is not a claim of an independently reproducible build or an audit.

The shared community wire verifier is authored in `community/protocol.mjs` and
copied with `node scripts/package-rooms.mjs sync-protocol /absolute/path/to/Sakura`.
Its adjacent browser declaration uses `.d.mts` for the `.mjs` module. The packaging
check requires both generated copies to match the reviewed source exactly.

After building the actual Sakura export, the complete community flow can be run
from the Sakura checkout with `node --test web/test/rooms-community.browser.mjs`.
It starts a temporary real service/database and uses the sibling `sakura-perps`
checkout (or the `SAKURA_PERPS_ROOT` override).
