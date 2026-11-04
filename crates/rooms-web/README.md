# Browser paper-room facade

This crate runs one fixed, signed Risk Rooms scenario in the browser. `Session`
is the host-testable implementation; `WebRoom` exposes the same implementation
through WASM. It reuses `rooms-auth`, `rooms`, and `risk` for authentication,
accounting, fees and payouts. It performs no signing, network requests, custody,
storage or real trades.

One browser-owned key is the owner, administrator and backing provider. Its
signatures demonstrate local authorization, not independent administration or
third-party oracle verification. The 1,000 credits of backing and each member
contribution are simulated accounting entries, not evidence of token deposits.

## API and units

The WASM exports are:

```text
WebRoom.create(keyHex, instanceNonceHex, planHashHex)
WebRoom.restore(genesisHex, recordsHexArrayJSON, expectedHeadHex)
room.genesis_hex()
room.records_json()
room.prepare(actionJSON) -> {step, intent_hex, signing_bytes_hex}
room.submit(intentHex, signatureHex) -> {record_hex, summary, checkpoint}
room.settle_scenario() -> {record_hex, summary, checkpoint}
room.summary_json()
room.checkpoint_json()
preview_allocation(actionJSON)
```

Structured WASM return values are JSON strings. Public keys, nonces and hashes
are exactly 32 bytes of lowercase hex; signatures are 64 bytes. Intent and
record bytes retain the unchanged authenticated-room canonical wire format.
Every numeric DTO value is a decimal string. Quote and SOL base amounts have
six decimal places: `1000000` quote atoms means one **paper credit**, with no
real-world peg. Displayed scenario prices are whole paper credits per SOL.
Input budgets and leverage are canonical whole-number strings, not floats.

`prepare` accepts only these objects, with no additional fields:

```json
{"action":"initialize"}
{"action":"allocate","budget_credits":"100","leverage":"3","side":"long"}
{"action":"run_scenario"}
```

Call and sign `initialize` four times, in order: register the fixed market,
register backing policy, fund the backing cell, then register the member. After
one allocation confirmation, prepare/sign the same allocation request twice:
the first contributes the requested budget; the second opens its position.
The supported budget is 1–100 credits, leverage is 1–3, and side is `long` or
`short`. No second contribution or position is admitted in this pilot.

The allocation budget **includes the entry fee**. Collateral is rounded down
to a 100-quote-atom increment so the SOL quantity has exactly the chosen
leverage at entry. The unused remainder stays in the member's free balance.
Fees and payouts are obtained from the economic kernel, including the preview.
The fee is 10 basis points of notional. The position's profit cap is 50% of
collateral, and its full maximum payout is reserved before entry.

The scenario has entry price 100 at slot 1, an indicative midpoint price 92 at
slot 2, and terminal price 104 at maturity slot 3. The midpoint payout is a
projection, not a completed close or a live executable quote. The scenario
has no borrow or funding charges. `run_scenario` prepares the administrator's
immutable terminal observation; after submitting it, `settle_scenario` uses the
stored price and beneficiary without consuming another signed sequence.
Neither path accepts arbitrary price, time, actor or beneficiary arguments.

Summary phases are `initializing`, `ready`, `funded`, `open`,
`terminal_recorded`, and `settled`. The browser should commit setup, allocation,
and terminal-settlement batches atomically, exposing stable phases `ready`,
`open`, and `settled` after their respective transactions. Checkpoints contain
`genesis_hash`, `record_count`, and `head_hash`.

## Plan binding and recovery contract

The genesis network is `SHA256("SakuraRoomsPaper/Network/v1\0")`. Its deployment
identifier is `SHA256("SakuraRoomsPaper/Deployment/v1\0" || instance_nonce ||
plan_hash)`. The plan hash commits the browser's canonical immutable plan
metadata. Free-text strategy statements are metadata, not new kernel policies;
only the fixed numeric scenario limits described above are enforced here.

The caller must canonicalize and hash the plan consistently. During restore,
recompute `create(publicKey, nonce, planHash).genesis_hex()` from stored metadata
and compare it with the stored genesis before accepting it. `restore` cannot
recover a nonce or plan from a hash and does not claim to verify unhashed text.

Every restore requires an expected head, validates the genesis shape, bounds
the history to eight records, and replays each record through both the pilot
restrictions and signature gateway. Valid signatures on different market
terms, paper funding, execution context or action order do not authorize them
in this facade. Changed outcomes, extra bytes, oversized frames, additional
records and truncated histories fail closed. No missing/corrupt store is
silently reset by this crate.

All in-memory mutation is only a **candidate** until the caller's IndexedDB
transaction atomically commits the complete new record batch and checkpoint.
Do not show success before that transaction completes. On persistence failure,
discard the candidate and replay the last committed snapshot. Concurrent tabs
need a single-writer mechanism or atomic compare-and-swap against the previous
checkpoint. A stale candidate must not overwrite a newer committed history.

A checkpoint stored beside its own history does not detect rollback of both.
An independent trusted checkpoint is needed for that guarantee. IndexedDB can
be cleared, evicted or restored; this facade provides no server backup,
consensus, blockchain commitment or browser power-loss durability guarantee.
Do not re-create an old genesis after storage loss and call that recovery.

Run host security tests with:

```sh
cargo test -p sakura-perps-rooms-web --locked
```
