# Invite-only community paper beta

This source adds `/app/perps/community` beside the existing local Rooms pilot.
Two or more people can join one immutable plan, approve their own allocation and
review their outcomes together. Each participant has a distinct browser-generated
Ed25519 identity and an independent paper ledger under that shared plan. There is
no collective custody, real wallet, live price feed or pooled member capital.

The supported exercise remains the fixed, finite capped SOL scenario: entry 100,
illustrative midpoint 92, final 104. The midpoint does not execute a stop or
liquidation. Each member selects 1–100 whole paper credits, including fees, and
1–3× long/short exposure. The existing Rust engine validates the canonical signed
records, fees and settlement in both browser and server. No second accounting
implementation was introduced.

## Authority and invitations

An operator provisions a one-use host admission code through the service CLI.
The creator signs an immutable plan and becomes its leader and first member.
Only that leader can issue or revoke invitations and revoke other members.
Invitations are random 256-bit secrets; the server journals only their hashes.
Links carry the secret in the URL fragment. The UI removes that fragment and
requires explicit join confirmation; it never sends an invitation automatically.

Every member signs their own allocation and settlement records. Neither the
leader nor the service signing key substitutes for a member key. Revocation
blocks new allocations and shared-history reads. A previously open allocation
can still be settled by its owner, and the owner retains access to their own
record without receiving future history belonging to other members. Member
identity bindings cannot be reassigned or reactivated in this beta.

HTTP commands bind the service identity, actor, request ID, validity window and
exact command. Published mutation signatures yield only their already-public
acceptance receipt if replayed. Reading private room history requires a fresh
signed read request. This prevents an exported request from becoming a reusable
credential for another member's history.

## Accepted actions survive restarts

The browser stores its nonextractable signing key, verified history checkpoints
and prepared request in strict IndexedDB transactions. It saves the exact signed
request **before sending**, blocks a second mutation while delivery is uncertain,
and retries that same request only when recovery is explicitly requested.

The dedicated Node service uses SQLite WAL with `synchronous=FULL` on persistent
local storage. Membership changes, complete six/eight-record paper batches,
single-use admission consumption and idempotency receipts commit atomically.
The HTTP acknowledgement follows the database commit. Multiple service instances
serialize writes through SQLite and compare the previous history before commit.
An accepted request ID cannot be reused with a different payload.

The browser verifies the service-signed acceptance event, retains its checkpoint,
then makes a separately signed read and replays the returned history. An HTTP
error alone never discards a pending approval. A terminal rejection must be
signed by the pinned service, bound to that exact request and durably recorded.
An uncertain response stays recoverable even when the original request expires.

Recovery preserves evidence of observed acceptance and revocation, including
when a revoked member receives only their own record. A later response must not
erase those stronger observations by returning an older, internally valid room.

## Trust and limits

- Browser keys control paper identities only. Losing one leaves cached history
  readable/exportable; it cannot be restored from a public export. No silent key
  replacement or account-recovery authority is provided.
- Each browser pins the service key on first connection and retains observed
  checkpoints independently. A changed service key or conflicting/shortened
  observed history fails closed. A new device cannot detect every server fork;
  this is not consensus, an independent witness network or proof of freshness
  after both browser state and service state are lost.
- The operator remains trusted for admission distribution, availability and
  request time. Hosting and same-origin JavaScript are trusted; nonextractable
  keys do not stop malicious same-origin code from requesting signatures.
- A persistent-volume service is required. Do not place this SQLite database on
  an ephemeral serverless filesystem or a network filesystem. Backups contain
  the service key and private room history and require private storage.
- The beta is bounded to 32 rooms, eight lifetime members and 64 lifetime invites
  per room, with bounded histories, request sizes, concurrency and rate limits.
  It is not a production-scale matching engine or a denial-of-service guarantee.
- A restored older backup must retain the same service identity and may be
  rejected by browsers that witnessed newer history. Operators must reconcile
  those checkpoints; resetting client state is not a safe workaround.

## Verification and operation

The [service runbook](../community/README.md) has initialization, admission,
serve, backup and restore commands. The [wire contract](../community/CONTRACT.md)
defines signatures and replay. Public source integration follows the existing
[allowlisted Expo package](../integrations/sakura-rooms/README.md); private Sakura
Studio source is excluded. Website publication still requires the Studio release
process and the required ATSU hosting restoration.

`npm --prefix integrations/sakura-rooms test` exercises protocol, SQLite/HTTP,
browser recovery and actual components against the shipped Rust WASM. CI repeats
the suite against a fresh build. Tests include two independent identities,
authorization attacks, invitation races, revocation, lost acknowledgements,
process death around commit, restart, backup restoration and client checkpoints.
The full Expo test at `web/test/rooms-community.browser.mjs` runs the same user
flow through the exported app at `/app/`, with desktop/mobile layouts and themes.

Before external access, provision persistent hosting, private backups, explicit
HTTPS origin routing, monitored operation and a verified private reporting
channel. No vulnerability mailbox, responder coverage, independent audit or
real-funds readiness is established by this merge.
