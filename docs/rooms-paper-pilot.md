# Rooms browser paper pilot

The 2026-11-04 source adds a local Plan → My allocation → Review flow. It uses
the real [Rooms kernel](../crates/rooms/README.md) and
[signed-command gateway](../crates/rooms-auth/README.md), compiled through
[`crates/rooms-web`](../crates/rooms-web/README.md) to WASM. The browser does not
implement a second accounting engine. Build and verification commands live in
the [public integration package](../integrations/sakura-rooms/README.md).

This is a paper experiment with one local identity acting as administrator,
member owner and backer. It connects no wallet, holds no assets, submits no
transactions and shares no room with another person. The source package and its
tests are not a record of a Sakura website deployment.

## What the screens do

1. **Plan.** Publish a title, thesis and invalidation condition. Canonical plan
   metadata is hashed into the immutable room domain, together with a fresh
   nonce. The browser creates a local Ed25519 key and signs the fixed setup.
   Editing the stored plan breaks recovery verification. The prose is a record
   of intent; it is not a machine-enforced trading strategy.
2. **My allocation.** Review and approve one long or short allocation, with a
   whole-credit budget of 1–100 and leverage of 1–3. The budget includes the
   entry fee. Integer calculations determine collateral, notional, fee, unused
   rounding balance and a profit cap of 50% of collateral. Contribution and
   opening are one persisted batch, so a partially saved allocation is not an
   accepted state.
3. **Review.** Run the known scenario: entry at 100 in slot 1, a displayed
   midpoint at 92 in slot 2, and settlement at 104 in slot 3. The midpoint is a
   projection, not a separate trade. The local administrator signs the fixed
   terminal observation, then permissionless settlement pays the stored
   beneficiary. Review shows the entry fee and capped result. Neither these
   prices nor the operator-controlled slots are live oracle evidence.

Each room permits one allocation and one settlement. The facade rejects
arbitrary prices, clocks, policies, extra actions and a second allocation,
including during replay of otherwise valid gateway records. Amounts cross the
WASM boundary as integer decimal strings; quote and base atoms have six decimal
places. The genesis, canonical records and checkpoint can be exported without
the private key.

## What a confirmed save means

The client first prepares and verifies a candidate state. It then writes the
whole action batch and checkpoint in one IndexedDB read/write transaction with
`durability: 'strict'`. It requires the browser to report strict durability and
acknowledges only transaction completion. A compare-and-swap check of the
previous stored state runs inside that same transaction; a competing tab's
stale write aborts instead of overwriting the winner. A failed or ambiguous save
requires recovery before another action.

Recovery bounds the input, checks metadata against the signed genesis, replays
every canonical record through the fixed facade and verifies the stored head.
Corruption, truncation, reordered records, mismatched plans and unsupported
versions fail closed. The client preserves the existing record instead of
silently resetting or discarding its tail. Browser tests exercise interrupted
batches and concurrent writes against the actual WASM engine and IndexedDB.

## Limits of local recovery

- IndexedDB durability relies on the browser, operating system and storage.
  It is not a replicated database, blockchain receipt or guarantee against
  device loss. Clearing site data, private-browsing cleanup or storage eviction
  can remove both the journal and signing key. An empty database cannot prove
  that no room existed before.
- The head is saved beside the records. An internally valid older snapshot of
  that same database cannot be distinguished from its original state without an
  independently retained checkpoint. Exported evidence helps only if its known
  checkpoint is preserved outside the database; it is not an automated witness.
- A missing or mismatched signing key makes a verified room read-only. Its
  history can still be reviewed and exported, but there is no key recovery,
  key rotation or restoration of signing access from a public export.
- The local key is non-extractable through WebCrypto, but code executing with
  the same origin's privileges can request signatures or modify local storage.
  The pilot does not protect against a compromised browser, application origin
  or operator. Signatures authenticate the selected paper identity; they do not
  prove custody, real market prices or independent settlement.
- The UI requires IndexedDB strict durability, WebCrypto Ed25519 and WASM.
  Unsupported capabilities fail instead of accepting an unsaved paper action.

The local [security challenge](security-challenge.md) covers these boundaries.
Public vulnerability intake remains unconfigured pending a verified private
channel and an assigned responder. No mainnet readiness, audit or bounty is
implied by this pilot.
