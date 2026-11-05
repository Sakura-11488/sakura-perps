# Community protocol v1

All hexadecimal values are lowercase. Identities, room IDs, request IDs and
hashes are 32 bytes; Ed25519 signatures are 64 bytes. Numeric transport values
are safe nonnegative integers. No token is a wallet credential.

Canonical browser endpoints are `GET /api/rooms/v1/info/` and
`POST /api/rooms/v1/command/`. The trailing slash avoids hosting-layer redirects;
the client continues to refuse every redirect. The dedicated service also
accepts the exact legacy no-slash paths. Query strings, extra path segments and
repeated slashes are not aliases.

`SignedRequest` has exactly `version:1, serviceId, actor, requestId, issuedAt,
expiresAt, command, signature`, in this canonical signing order (signature is
excluded from the signing preimage). The optional HTTP `credential` is a
32-byte secret token, outside the signature. Its SHA-256 digest must equal the
signed command's `credentialHash`. It is accepted only for create/join and is
never journaled or returned in public history. Signature domain:
`SakuraRoomsCommunity/Request/v1\0` + UTF-8 canonical JSON.

Commands (exact keys, no extras):

- `create_room`: `type, roomId, plan, credentialHash`
- `create_invite`, `revoke_invite`: `type, roomId, inviteHash`
- `join_room`: `type, roomId, credentialHash`
- `revoke_member`: `type, roomId, member`
- `get_room`, `get_my_record`: `type, roomId`
- `commit_ledger`: `type, roomId, expectedHead, ledger`

`plan` is `{title,thesis,invalidation}` with existing pilot limits 80/1000/500;
NFC, trimmed, nonempty. `ledger` is `{genesis,records,head}` with exactly 6
(open) or 8 (settled) bounded canonical Rust records. `expectedHead` is null for
the first 6-record commit and the exact prior ledger head for 8-record settlement.
No 4-record intermediate ledger is persisted by the shared service.

Each journal event is `{index,previousHash,request,hash,signature}`. Index starts
at 1; the initial previousHash is 32 zero bytes. `request` is the complete signed
request without credential. `hash` is SHA256(`SakuraRoomsCommunity/Event/v1\0`
+ canonical JSON of `{index,previousHash,request}`). The service signs UTF-8
`SakuraRoomsCommunity/EventSignature/v1\0` + the lowercase hash.

`replayRoom(engine, serviceId, events, checkpoint?)` verifies all signatures,
roles and transitions and returns `{version:1,serviceId,roomId,plan,leader,
members:[{publicKey,status:'active'|'revoked',ledger:null|Ledger}],
invites:[{hash,status:'active'|'claimed'|'revoked',claimedBy:null|string}],
head,events}`. A checkpoint `{count,head}` pins a prefix: shorter histories or a
different hash at that count are rejected. It is independently retained by each
browser; it cannot reveal server equivocation to a brand-new device.

The creator is the leader and first active member. Only the leader creates or
revokes invites and revokes members; the leader cannot revoke itself. Invites
are single-use and one hash cannot be reissued. Members cannot rejoin under the
same identity after revocation. Caps: 8 lifetime members, 64 lifetime invites,
256 events. Duplicate actor/requestId pairs in one room are rejected.

Every member's Rust genesis is `WebRoom.create(memberKey,roomId,planHash)`, where
planHash = SHA256(`SakuraRoomsCommunity/Plan/v1\0` + canonical JSON
`{version:1,serviceId,roomId,leader,plan}`). The service identity is included to
prevent cross-service reuse. An initial commit must contain 6 records. A later
commit must contain 8, preserve the previous six bytes-for-byte and use its exact
head. The Rust facade verifies actions, operator context, signatures and result.
Revocation blocks new allocations; an already-open own ledger may still settle.
No leader or service signing key substitutes for the member key.

Request validity windows are at most five minutes. Live service admission checks
the server clock (30s future tolerance); historical replay checks shape and
signatures without comparing to today's clock. Durable idempotency is server
responsibility: an identical actor/requestId request returns its committed
result without a new event; reusing that ID with different signed bytes fails.
Public journal replay cannot independently establish server admission-token
distribution or wall-clock truth.

HTTP mutations return only `{version:1,kind:'receipt',serviceId,roomId,event}`,
including idempotent retries. The event is the exact accepted request, signed
by the service. It contains no other member's history. Shared history requires
a fresh signed `get_room`; a former member can request `get_my_record`, which
contains its own ledger, signed creation/commit/revocation proofs and no other
member's later activity. Publishing a signed mutation must not turn it into a
reusable bearer credential for confidential history.

A terminal refusal is actionable only with a service-signed rejection:
`{version:1,serviceId,actor,requestId,requestHash,expiresAt,code,signature}`. The service
signs `SakuraRoomsCommunity/Rejection/v1\0` + canonical JSON of the fields except
signature. `requestHash` is SHA256 of the Request domain plus the entire
canonical signed request (credential omitted). The browser verifies the service,
actor, ID, request digest and exact request expiry before clearing a prepared mutation. The signed expiry also protects the server's rejection-retention metadata from an undetected change. Unsigned HTTP
errors and network failures cannot establish non-acceptance. The service must
durably arbitrate accepted/rejected outcomes for a request before issuing this
receipt; a signature alone does not implement that storage rule.
