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

**One claim in the design is inferred rather than executed:** that a third party
can refresh the market's pinned Pyth account. `verify-push.ts` is the experiment
that settles it. Run that before trusting anything else.

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

**The keeper earns nothing on the positions that matter most.**
`apply_liquidation_fee` clamps the fee against what the close fee left of the
payout, so once a position is far enough underwater the fee, and the keeper's cut
of it, is zero. Those are precisely the positions generating bad debt. This is a
property of the protocol's incentive, not of this bot, and it means §9.4's
bad-debt overhang is only partly addressed by permissionless liquidation.

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
