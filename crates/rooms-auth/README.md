# Signed Risk Rooms commands

This crate adds a real Ed25519 authorization boundary around the in-memory
[Risk Rooms paper kernel](../rooms/README.md). It verifies who authorized an
action and the exact terms they signed, then applies that action atomically with
its replay counter and audit record. It is an off-chain research prototype,
without token custody, authenticated live oracles, durable storage or consensus.
The separate [browser paper pilot](../../docs/rooms-paper-pilot.md) persists a
restricted scenario in IndexedDB around this crate; its local journal still
requires an independent checkpoint to detect rollback of a valid database.

```sh
cargo test -p sakura-perps-rooms-auth --locked
cargo run -p sakura-perps-rooms-auth --example signed_room --locked
```

The example uses public deterministic test seeds. They are not wallets and must
never control real assets. It demonstrates signed setup and trades, altered
messages, replay, delegation revocation/regrant, owner close, permissionless
maturity settlement and transcript verification. Every expected rejection is
asserted rather than merely printed.

## What a signature binds

A genesis specifies network and deployment identifiers, the room ID, the
administrator ID, and an immutable registry of owner/delegate IDs and Ed25519
public keys. The caller must independently trust this genesis. The registry
supports at most 128 principals, is strictly ordered by role and ID, and rejects
duplicate identities, reused keys, weak keys, noncanonical encodings and keys
outside the prime-order subgroup. The administrator must have an owner binding.
An administrator cannot reassign anyone's key. There is no key rotation or lost
key recovery in this version.

Every signed intent contains:

| Field | Purpose |
|---|---|
| `genesis_hash` | Binds the complete registry, network, deployment, room and protocol version. |
| `actor` | Distinguishes owner and delegate roles as well as numeric identity. |
| `sequence` | The actor's next one-time transport sequence, across all its signed actions. |
| `not_before_slot`, `expires_slot` | Inclusive execution window, at most 10,000 slots. |
| `authorization_revision` | Current member policy revision for every open/close; absent for other actions. |
| `action` | Complete typed action, including amounts, recipients' member/cell IDs, policy values and economic intent. |

Ed25519 signs the complete canonical intent bytes with the `Intent` domain and
version prefix. Verification uses `ed25519-dalek::VerifyingKey::verify_strict`.
The pinned library documents its
[strict signature and weak-key checks](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/struct.VerifyingKey.html#method.verify_strict).
Every actor-bearing operation is signed, including contributions, cell funding,
withdrawals, policy changes and terminal-price publication. A valid signature
does not bypass the kernel's owner/delegate or economic checks.

The wrapper constructs the economic `Actor` from the signed intent. Callers
cannot supply another actor through execution context. It owns a private `Room`,
starts only from genesis, and exposes no mutable kernel or unsigned owner path.
Direct use of the separate kernel's `Room::execute` still has asserted identities
and does not acquire the wrapper's authentication guarantees.

## Delegation, cancellation and replay

Transport sequences are independent of the kernel's per-member trade nonces.
Each successful signed action consumes exactly one actor sequence; an error
consumes neither counter and leaves the room, clock, receipts and hash head
unchanged. Independent actors have independent sequences. A shared delegate's
actions across members are ordered by that delegate's single sequence.

`CancelPending` signs and consumes the actor's current sequence without changing
economic state. This invalidates a held request at that sequence. It does not
cancel every future sequence or an already executed action. It wins only if
executed before the competing request.

Opening signatures retain all kernel policy, underwriting, price, generation,
nonce and maturity constraints. A close binds its position's original economic
revision **and** the member's current authorization revision. Revoking and later
regranting the same delegate advances the latter, so a previously signed close
cannot become valid again. An owner can create a new close authorization for
the original economics after changing policy or removing the delegate.

Creating a new wrapper from genesis resets its replay counters. A process must
restore the complete verified transcript and an independently trusted checkpoint
before accepting requests after a restart. This crate does not persist that
checkpoint, prevent concurrent forks or provide crash-safe execution. A new
deployment/season must use a distinct domain; copying a genesis and resetting
state is not a safe restart procedure.

## Clock, prices and paper funding

`ExecutionContext` is a trusted adapter input. Its monotonic current slot and
live observation are deliberately absent from trader-supplied signed actions.
The adapter must obtain them independently; an HTTP service must not copy these
fields from a user's request. Open/close require an observation and other signed
actions reject one. Kernel price freshness, market generation and signed price
bounds still apply.

An administrator signs terminal observations, which remain immutable and bound
to the captured maturity slot. This authenticates the publisher, not the truth
of the price. Anyone may call `settle_matured(position, operator_now)` using the
stored terminal price. It has no signer, price or beneficiary input and consumes
no actor sequence or member trade nonce. Missing terminal data still locks the
reserve; permanent-outage recovery remains unresolved.

Contributions and cell funding are signed **paper accounting actions**. A
signature is not proof that collateral arrived. A real integration must couple
them to independently verified, single-use custody receipts or atomic token
transfers. Likewise a recorded withdrawal does not execute a token transfer.

## Canonical wire format v1

The codec uses Borsh 1.8 with strict collection ordering and a 16 KiB limit.
Decoding rejects trailing data, truncated values, unknown enum tags, duplicate
or unordered set elements, and any value that does not re-encode exactly.
Encoding uses a bounded writer rather than allocating an unlimited intermediate.
Each policy allowlist has at most 64 entries so complete execution records also
fit the frame limit. Collection lengths remain explicit little-endian `u32`
counts, integer fields are fixed-width little-endian, and no floating-point or
Rust Debug/JSON representation participates in signing.

The preimages are:

```text
genesis_hash  = SHA256("SakuraRiskRooms/Genesis\0" || u16_le(1) || Borsh(genesis))
signing_bytes =        "SakuraRiskRooms/Intent\0"  || u16_le(1) || Borsh(intent)
record_hash   = SHA256("SakuraRiskRooms/Receipt\0" || u16_le(1)
                      || Borsh((index, previous_hash, evidence, outcome)))
```

The initial hash head is `genesis_hash`. An envelope is Borsh
`{ intent, signature: [u8; 64] }`; protocol v1 is fixed by this codec and its
signature preimage. Signature bytes are not a custom prehashed signature scheme.
Changing field order, tags or semantic interpretation requires a new protocol
version and an explicit migration. Fixed golden-byte tests pin this format.

Actor tags are owner `0`, delegate `1`; each is followed by its `u64` ID. Position
side tags are long `0`, short `1`. The stable action tags are:

| Tag | Action |
|---|---|
| 0 | RegisterMarket |
| 1 | AddCell |
| 2 | ChangeCellPolicy |
| 3 | AddMember |
| 4 | ChangePolicy |
| 5 | Contribute |
| 6 | FundCell |
| 7 | Withdraw |
| 8 | WithdrawCell |
| 9 | Open |
| 10 | Close |
| 11 | RecordTerminalObservation |
| 12 | CancelPending |

## Receipts and evidence

Each record contains its index, previous hash, signed envelope plus operator
context (or a permissionless settlement request), computed outcome and hash.
`AuthenticatedRoom::replay` verifies signatures, counters, authority, transitions,
every recorded output and the final expected head. It rejects altered receipts,
reordered records and a truncated history when given the original trusted head.

The expected genesis and head must come from independent trusted storage or a
checkpoint mechanism. A rewritten history presented with its own replacement
head is not authenticated merely because its hashes reconcile. Operator time
and price context are recorded but not attested by the trader's signature.
There is no operator signature, Merkle inclusion proof, on-chain commitment or
consensus protocol here. Full-state cloning and transcript replay still grow
with the history; this is not a bounded-compute on-chain design.

This narrows the remaining work to custody, authentic price/time adapters,
durable checkpointing, key lifecycle, settlement recovery, wallet-facing signing
UX and independent review. It does not solve the original perpetuals program's
LP-exit or funding-insolvency design gates.
