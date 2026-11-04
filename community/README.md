# Invite-only community paper Rooms

This service lets invited people follow one immutable plan and approve their
own independent paper allocations. It uses the existing signed Rust Rooms
engine through the same content-pinned WASM as the browser. There is no second
JavaScript accounting engine, shared custody, real deposit, live order, or
market-price oracle. The known SOL scenario remains a workflow exercise.

Each browser has its own nonextractable Ed25519 paper identity. A leader can
create/revoke invitations and revoke membership. It cannot sign, enlarge,
redirect, or change another member's allocation. There is no key rotation or
lost-key recovery: losing browser site data loses signing access. Public exports
preserve evidence, not the private key.

## Run locally

Use Node **22.23.x** and the committed integration engine. This is a dedicated
long-running service on a **persistent local filesystem**. SQLite storage must
not be placed in an ephemeral serverless function filesystem or shared through a
network drive. The [`node:sqlite` API](https://nodejs.org/download/release/v22.23.0/docs/api/sqlite.html)
is still marked experimental in Node 22; keep the runtime pinned and rerun the
tests before upgrading. There are no service runtime npm dependencies.

Choose a private data directory outside source control. Initialization is an
explicit operation and refuses to overwrite any existing file:

```sh
node community/server.mjs init --db /private/sakura-rooms/rooms.sqlite
node community/server.mjs issue-admission \
  --db /private/sakura-rooms/rooms.sqlite \
  --out /private/sakura-rooms/admission.txt
node community/server.mjs serve \
  --db /private/sakura-rooms/rooms.sqlite \
  --port 8090 \
  --app-dist /absolute/path/to/Sakura/web/dist
```

The complete Expo export must already be built with Sakura's root `npm run build`.
Open `/app/perps/community` on the service origin. The optional static
server maps the export to `/app/`; it does not proxy other Sakura backend APIs.
An operator gives the admission token privately to an approved room creator.
It expires after 24 hours and creates one room. The CLI writes it only to the
requested new file with mode `0600`; it never prints the token. A creator can
then create single-use member invitations in the UI. Invitation tokens are
bearer capabilities: anyone who obtains one can claim it before it is revoked
or used. Share them only with the intended people. They have no automatic expiry
in this beta; the leader can revoke an unclaimed invitation.

The database, including its service signing key, is private (`0600`). SQLite
uses WAL mode, `synchronous=FULL`, foreign keys, strict tables, and a five-second
busy timeout. Startup verifies the identity, database integrity, complete signed
histories, receipt indexes, rejection proofs and admission bindings. Missing,
incompatible or corrupt storage stops startup; it is never recreated as an empty
service. The service's public identity must remain stable across restarts.

For remote use, place TLS termination in front of this process and supply the
exact HTTPS origin with `--origin https://your-approved-beta.example`. Bind an
external interface only explicitly with `--host`. Forward the original canonical
Host; the service does not trust `X-Forwarded-Host`, `X-Forwarded-For`, or arbitrary
CORS origins. Keep the private database off the web root. This repository does
not provision hosting, register a domain, expose a listener publicly, or deploy
Sakura. Its private Studio and ATSU release requirements still apply separately.

## Durable requests and recovery

The protocol is documented in [CONTRACT.md](CONTRACT.md). All room reads require
a clock-valid signed request from an authorized identity. All state changes bind
the service, actor, room, canonical command, request ID and expiry. The paper
ledger's own signed genesis also binds the community plan and member.

The browser retains its exact pending request before transmission. The server
verifies a disposable candidate, then uses one SQLite transaction for the
journal, invitation/admission consumption, request receipt and checkpoint.
There is no asynchronous gap inside that write transaction. A previous-state
comparison also protects against competing service processes. Success is
acknowledged only after `COMMIT`.

An identical request ID and signed body returns the original receipt even after
expiry. A reused ID with different signed bytes is refused. Mutation responses
contain **only that mutation's signed event**. A public export includes signed
mutation requests, so those signatures must never become read credentials for
another member's history. The browser makes a separate fresh signed read after
acceptance. Revoked members cannot read the current shared history or approve a
new allocation, but they can obtain their own public record and settle their own
already-open paper allocation through the API. Revocation cannot erase history
someone already received.

A terminal rejection is useful only if it cannot later become an acceptance.
For valid signed mutations, the service commits a rejection tombstone and signs
the exact request hash, identity, expiry and rejection code. The browser can then
clear that pending request. An unsigned proxy error, timeout, transport rejection
or unavailable service remains ambiguous and must preserve the pending request.
Accepted receipts always take precedence over a racing rejection. Tombstones
are retained through the request expiry plus 30 seconds, and pruning happens
inside a write transaction. A durable clock floor prevents a backwards system
clock from making pruned requests valid again. An arbitrarily future-dated packet
does not receive a durable rejection; correct the client clock and recover.

Server time and the persistent filesystem remain trusted. Whole-database rollback
can roll back the clock floor and receipts too. Browser-retained service identity
and history checkpoints detect a shorter history or a changed known prefix;
they do not establish global consensus, prevent denial of service, or expose
equivocation to a brand-new browser without a trusted prior checkpoint. A
compromised application origin/browser can use its nonextractable key to request
signatures. A compromised service host can access the service key and private
room data. These limits are not resolved by passing tests.

## Backup and restore

Use SQLite's online backup API rather than copying a live database and hoping its
WAL matches:

```sh
node community/server.mjs backup \
  --db /private/sakura-rooms/rooms.sqlite \
  --out /private/sakura-backups/rooms-2026-11-04.sqlite
```

The destination directory must already exist and be private. The command
exclusively creates a new `0600` file, performs an online SQLite backup, then
reopens and verifies its complete history and service identity. It never
overwrites an existing backup. Backups include the service signing key and
private membership/plan history: protect and encrypt off-device copies under
the operator's normal backup policy. They contain no browser private keys.

For a restore rehearsal, stop the writer, retain the original database, and
start the service against the verified backup's explicit path. Do **not** run
`init` on a backup and do not delete browser checkpoints to make an old backup
appear current. Keep the same public origin and service identity. Clients that
have seen a newer checkpoint must reject an older restored history; reconcile
the missing accepted records before resuming. A backup proves that snapshot,
not that it contains every later acknowledged action. Maintain a tested backup
schedule and an independently retained recent checkpoint before public exposure.

## Bounds and operating gates

The beta admits at most 32 rooms, 128 lifetime creator admissions, 8 lifetime
members per room, 64 lifetime invitations, and 256 events per room. Admission
slots are not silently recycled. Rejections are capped at 1024 concurrent
tombstones; when full the service fails ambiguously instead of acknowledging a
rejection it cannot retain. HTTP bodies are bounded to 512 KiB with a five-second
read deadline. Protocol inputs have smaller structural caps; room history is
bounded to 4 MiB. IP and verified-identity rate limit maps are bounded, connections
and in-flight responses are capped, and idle sockets time out. Behind one proxy,
the IP limit applies to that proxy as well; tune deployment routing deliberately.

The service emits only startup/operation status, never request bodies, capability
tokens or key material. There is no automatic restart/recreation on corruption.
Preserve the private volume, stop admission during an incident, retain relevant
checkpoints and backups, and investigate before restarting writes. No public
security challenge should be announced until a private reporting channel and
an accountable responder are verified; that operational gate remains open.

Run service and protocol tests with:

```sh
node --test community/*.test.mjs
```

Tests use real SQLite, real signatures and the actual Rust WASM engine, including
two writers, process `SIGKILL` before/after commit, restart, immutable identity,
own-only ledger authorization, revocation, public-signature read isolation,
signed rejection replay, malformed transport, backup/restore and checkpoint
rollback detection. Full Expo two-browser tests live in the integration bundle.
The beta remains unaudited and unsuitable for funds of real value.
