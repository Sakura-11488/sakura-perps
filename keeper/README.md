# sakura-perps keeper

An off-chain bot that finds underwater positions and calls `liquidate_position`.

Devnet only. Unaudited. It signs transactions with a hot key — run it with a
wallet you are willing to lose.

## Status, plainly

**The bot is live and correct, and has nothing to do.** Devnet currently has an
Exchange and a Pool and *zero* markets, feeds and positions. So there is nothing
to liquidate, and there will be nothing until somebody runs
`qualify_feed → create_market → set_risk_params → open_position`.

That matters more than it sounds: on an empty book, a perfectly working keeper
and a completely broken one both print "nothing to do". `preflight.ts` exists to
tell those apart, and it is the artifact worth running today.

## Blocked: the price feed source wants credentials

`verify-push.ts` was run for real on 2026-08-26 and **did not push**. Pyth's
Hermes endpoint — free and unauthenticated for years, and what every Pyth example
still shows — now answers **401 unauthorized**:

    $ curl -s -D - "https://hermes.pyth.network/v2/updates/price/latest?ids[]=ef0d8b6f…"
    HTTP/1.1 401 Unauthorized
    x-infra: platform-yellow
    unauthorized

That is Pyth's own service refusing, not a network block: from the same host,
Solana RPC and GitHub both return 200, and `hermes-beta.pyth.network` refuses
identically. So the keeper cannot currently fetch a price update to carry, which
means it cannot refresh the pinned account, which means it can only liquidate
during whatever windows the sponsored publisher happens to leave fresh.

**To unblock:** set `HERMES_URL` to an endpoint you have access to and
`HERMES_AUTH` to its header value (for example `Bearer sk-…`). Neither is ever
logged. Then re-run `npm run keeper:verify-push` — success is
`publish_time` and `posted_slot` both advancing, **not** a confirmed signature.

What the run did establish, before the 401:

- The shard-0 PDA for SOL/USD derives to `7UVimffx…` — the account a market
  would pin — so the write target is right and the design is structurally sound.
- The feed is badly stale in practice. Two runs about two minutes apart measured
  age **167 s / 945 slots** then **299 s / 1749 slots**, with no write in
  between. Against a 120 s / 300-slot guard it was far outside both, the whole
  time. This is the reason the bot carries its own update rather than waiting.

**Still unproven:** that a third party can actually write to that account. It is
permissionless by construction and every observed write came from an ordinary
wallet, but we have not executed one. Treat the atomic oracle strategy as
plausible-and-unverified until `verify-push` reports the feed advancing.

## Quick start

```bash
export RPC_URL=...        # a real RPC; the bot refuses any non-devnet genesis
export KEYPAIR=~/.config/solana/keeper.json

npm run keeper:preflight      # sends nothing, proves the bot is alive
npm run keeper:verify-push    # dry run; PUSH=1 to actually push a price
npm run keeper:run            # DRY_RUN defaults ON — set DRY_RUN=0 to act
```

## How it decides to liquidate

**It asks the program.** Every candidate is put through `simulateTransaction`
with the exact instructions that would be sent. A clean simulation means
liquidatable; `Custom(6062)` means healthy; anything else is a problem to report,
never a quiet skip.

The alternative — porting the risk maths to TypeScript — was rejected. It would
mean reimplementing `accrue`, `clamp_to_ema_band`, `execution_price`,
`value_close`, `notional_usd_ceil`, `margin_requirement` and `is_liquidatable`,
each with a deliberate floor-or-ceil direction, and each wrong exactly at the
boundary where every liquidation lives. The program is already the authority; a
second implementation is a second thing to keep correct.

`rank.ts`-style scoring does not exist for the same reason. Positions are
simulated in the order they are found, capped at `MAX_SIMS_PER_TICK`.

## The oracle problem

`LiquidatePosition` pins its price account:

```rust
#[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
pub price_update: Box<Account<'info, PriceUpdateV2>>,
```

so the keeper **cannot** post its own fresh price account and pass that. It must
write into the market's own account — which, for a Pyth push feed, is the shard-0
PDA. Verified: `getPriceFeedAccountAddress(0, SOL/USD)` derives
`7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`, the feed this repo's README
names, owned by the Pyth receiver and writable by anyone.

This is not optional. Devnet's sponsored publisher is intermittent — measured
going **314 seconds** between writes, with slot age reaching ~1500. Against a
300-slot liquidation guard at devnet's measured **~6 slots/second** (≈50 seconds,
not the ~120 seconds a 400 ms slot assumption implies), a keeper that waits for
someone else to push can only act during a small minority of each blackout, in
windows it does not choose.

So the bot carries its own price update **in the same transaction** as the
liquidation. Same transaction, not a prior one: it removes the race between
refreshing and acting, and it means the keeper never pays to publish a price
unless it is being paid to liquidate. A keeper that pushes on a timer is just an
unpaid Pyth publisher.

## The economics, including the bad part

The keeper is paid `exchange.keeper_fee_share_bps` (currently **2000** = 20%) of
the liquidation fee. That fee is charged either way — the keeper's share comes
*out of* it, never on top — so a liquidated trader pays exactly what they would
have paid the admin path.

**The keeper earns nothing on a position that is already deep underwater when it
finds it — measured, not predicted.** `apply_liquidation_fee` clamps the fee
against what the close fee left of the payout, so past a certain depth the fee,
and the keeper's cut of it, is zero. How often that happens is a function of how
promptly the bot acts, not of the protocol: see the section below.

The first real permissionless liquidation on devnet (2026-08-30) paid exactly
that:

    gross_payout_quote     0
    close_fee_quote        0
    liquidation_fee_quote  0
    net_payout_quote       0
    bad_debt_usd           1,690,280

The keeper paid gas and earned nothing, on a position that booked $1.69 of bad
debt. Treat a zero fee as the expected case for anything deeply underwater, not
as a rare edge.

### Polling is enough to get paid — an earlier version of this file said otherwise

This section used to be headed "the bot MUST crank `settle_market`, not just
watch", and claimed a polling keeper always arrives after the fee has clamped to
zero. **That was wrong**, and it mattered: it described the bot's central design
as unprofitable when it is not.

`handle_liquidate_position` calls `accrue` at step 2, *before* the solvency gate.
So a stale stored index protects nobody. The simulation this bot runs accrues to
the current cluster clock, sees the position cross the boundary on the very next
tick, and the liquidation that follows charges the ordinary fee.

`a_stale_market_index_does_not_stop_a_keeper_being_paid` in the SVM suite proves
it: six hours with nothing touching the market, stored index asserted unchanged,
then a liquidation with a non-zero fee that the keeper is paid.

The error came from `accrue`'s doc comment, which listed five callers and omitted
`liquidate_position`. Reading that as exhaustive is what produced the claim.

The devnet liquidation paid zero because nobody was watching for three days and
the market was manually settled immediately before it was liquidated — the
backlog landed in one step and carried the position past the fee-payable window.
A keeper running on a tick would have caught it at the boundary.

### The crank is still worth running, for the pool rather than the keeper

`accrue` clamps to `dt = min(elapsed, max_settle_window_seconds)` while
`last_settle_ts` advances to now regardless, so time beyond one window is
**forgiven, not deferred**. An unattended market under-charges borrow interest
and the shortfall lands on the LPs.

That is a reason for *someone* to crank on a cadence. It is not a reason this bot
must, and it changes no liquidation economics. `run.ts` does not do it; adding it
is cheap if wanted — permissionless, `pool` and `market` only, and a no-op when
`now == last_settle_ts`.

`SUBSIDISED=1` liquidates at a loss for operators who care about pool solvency
more than the fee. Off by default: a bot that quietly pays to work is a bot
nobody notices burning a wallet.

## Safety rails

| Rail | The failure it prevents |
|---|---|
| Only `Custom(6062)` means healthy | One misderived account making every simulation fail, so the bot reports an empty book forever while completely broken |
| Oracle codes are **market-wide**, never per-position | An oracle outage being recorded as "every position is solvent" |
| Boot asserts 6062 is `PositionNotLiquidatable` | An IDL/program mismatch silently inverting every verdict. These discriminants have already shifted once in this program's history |
| `DRY_RUN` defaults on | A first run spending money while account derivations are unproven |
| Genesis-hash refusal | A mistyped `RPC_URL` arming an unaudited keeper against mainnet |
| Re-simulate the exact signed bytes | The measured transaction and the sent transaction differing after the compute-budget rewrite |
| Lock released only on ground truth | Double-sending a liquidation whose first transaction is still confirming |
| Daily lamport ceiling, per-tick and per-hour caps, failure breaker | A runaway resend loop burning the wallet overnight |
| SOL floor | Draining past the point where the keeper can pay for the transaction that reports it is stuck |
| Priority fee capped at a share of the measured fee | Winning a race at negative EV |
| Cluster time, never `Date.now()` | VPS clock skew mis-sizing every age decision against a ~50-second window |
| `KEYPAIR` is a path; every log line is redacted | Key material in an env var visible to `ps`; the RPC api-key leaking into logs |
| Never push without a liquidation attached | Becoming an unpaid Pyth publisher |
| Missing owner token account is a hard skip | Spending ~0.002 SOL of unrecoverable rent to fund an account the *trader* owns |
| `HALT` file stops sending, keeps observing | A circuit breaker also blinding the operator |

## Deliberately not built

Batching, websockets, owner-ATA creation, a database, a dashboard, Jito bundles,
multi-market sharding, a mainnet profile. All of it is surface area unearned by a
book of zero positions. `buildLiquidateIx` returns a bare instruction and
`loadPositions` is one function, so each is a contained change when a real book
justifies it.

Market bootstrap (`qualify_feed` / `create_market` / `set_risk_params`) is
admin-gated and belongs in its own script, not in a keeper.

The `settle_market` crank is also not built, and that IS a choice — though this
file previously said the opposite, calling it "not a choice" and a gap that made
the bot unprofitable. It does not: liquidation accrues before its own gate, so
the keeper is paid without it. Cranking benefits the pool's interest take, which
is the LPs' concern and not this bot's job. See the economics section.

## Telling success from a no-op

A confirmed signature is **not** success. Losing the race to another keeper also
confirms, having done nothing. The bot checks ground truth: `close = owner`
deallocates the position, so its absence from `getAccountInfo` is the only proof
that a liquidation happened. Look for `event: "liquidated"` in the log, not for a
transaction id.

Two numbers must be chosen correctly at `qualify_feed`, permanently —
re-qualification is unsupported:

- `expected_exponent` must match the feed (**−8** on the live SOL/USD feed). A
  mismatch is the first guard checked and fails before anything is scaled.
- `liquidation_max_age_slots` must be sized against devnet's measured ~6
  slots/second, not the 400 ms assumption behind the comment at
  `crates/risk/src/oracle.rs:105-130`.

## Files

| File | Contents |
|---|---|
| `src/config.ts` | Every tunable, named once. `loadKeypair`, `redactRpc` |
| `src/program.ts` | Connection, `Program`, PDAs, genesis refusal, cluster clock |
| `src/errors.ts` | The verdict classifier and its boot assertion |
| `src/oracle.ts` | `PriceUpdateV2` decoding, staleness, Pyth push instructions |
| `src/discover.ts` | Positions, markets, owner token account resolution |
| `src/economics.ts` | Fee measured from simulation, profitability gate |
| `src/liquidate.ts` | Build, two-phase simulate, verified send, ground truth |
| `src/guard.ts` | Locks, budgets, breakers, `HALT` |
| `src/observe.ts` | Redacted JSONL logging and the healthy-only heartbeat |
| `src/run.ts` | The tick loop; the only file with control flow |
| `preflight.ts` | Proves liveness without sending. Run this first |
| `verify-push.ts` | Milestone zero: can a third party refresh the feed? |
