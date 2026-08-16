# Sakura Perps — Milestone 5 Specification

### Markets and positions

Program `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` · Anchor 1.1.2 · devnet only, pre-audit
Baseline: `devnet-v0.5.0` plus the three post-v0.5.0 fixes on `main`
(`cancel_withdraw`, the exact utilisation ceiling, `require!(gross > 0)`).

> ## Provenance — read this first
>
> The original of this document was written on 2026-08-01, survived an
> adversarial refutation pass, and was then **lost** when a scratchpad was
> cleaned. It was never committed. This file is a partial reconstruction.
>
> Every section is tagged:
>
> * **[RETAINED]** — recovered verbatim from the original. Trust as written.
> * **[RECONSTRUCTED]** — rebuilt from the account layouts the original
>   produced (which are authoritative, since they were implemented from it),
>   from the retained sections, and from the section titles of the refutation
>   log — which survived even where their arguments did not.
>
> A **[RECONSTRUCTED]** section has *not* been through refutation. Treat its
> reasoning as a proposal, not as settled. Sections 3.9, 3.10, 5.5 and 5.6, and
> all of §2, are **[RETAINED]** and carry the original's authority.
>
> This file is committed. That is the point.

---

## 1. SCOPE — [RETAINED]

Milestone 5 is the on-chain plumbing that turns `crates/risk` into a venue: a
`QualifiedFeed` (admin-written oracle allowlist entry), a `Market`
(permissionlessly listed against a qualified feed, quarantined at zero open
interest until an admin sets its risk parameters), a `Position` (isolated
margin, one per owner per market, no adds), and the instructions that open,
settle and close one. It also arms and closes five holes that only become
reachable once `reserved_quote` and `locked_quote` stop being structurally zero
— `cancel_withdraw`, `set_max_aum`, the reserve-time utilisation ceiling, a
`require!(gross > 0)` on `lp_withdraw`, and an exact-rational form of
`withdrawal_leaves_enough_reserve`.

It is **not** liquidation-by-keeper (M7 — M5 ships only `admin_settle_position`
as a stopgap), **not** cross-margin, **not** limit orders or TP/SL, **not**
adds-to-position (`blended_entry_price` stays uncalled on chain), **not** an
insurance fund (bad debt is recorded, never socialised), and — the change forced
by refutation — **not** a rewrite of LP share pricing. AUM stays
`pool.quote_deposited` exactly as it is today.

---

## 2. ACCOUNTS — [RETAINED], and already implemented

`QualifiedFeed`, `Market` and `Position` are implemented at `2749aaf` in
`programs/sakura-perps/src/market.rs` and `position.rs`, and `Pool` gained
`min_liquidity_quote` carved from `_reserved`. The field tables in the original
were followed exactly; consult the source, which is now the record.

The three properties worth restating because §3 depends on them:

* `QualifiedFeed` carries **every** risk-bearing oracle parameter, including the
  pinned `price_update` account. A market creator chooses a feed and nothing
  else.
* `max_oi_usd == 0` **is** the quarantine, read directly rather than mirrored
  into a flag.
* `Position` snapshots `maintenance_margin_bps`, `liquidation_fee_bps`,
  `close_fee_bps` and `reserve_quote` at open.

---

## 3. INSTRUCTIONS

### 3.1 `qualify_feed(params: QualifyFeedParams)` — admin — [RECONSTRUCTED]

Seeds `[b"feed", params.feed_id]`, `init`. Admin-only, asserted against
`exchange.admin`.

Writes every field of `QualifiedFeed` from `params`. Validation, all before any
write:

1. `expected_exponent` within `MIN_EXPONENT ..= MAX_EXPONENT`.
2. `min_price > 0` and `min_price < max_price` — a band that cannot contain a
   price makes every trade fail closed, which is safe but silently unusable.
3. `asset_decimals <= MAX_POW10`, so `pow10` cannot fail downstream.
4. `validate_guard_ordering(&trading, &liquidation)` — §3.10. Trading guards
   must be no looser than liquidation guards on every axis, or a position opens
   at a price the protocol will not liquidate against.
5. `price_update` is deserialised as a `PriceUpdateV2` and its `feed_id` must
   equal `params.feed_id`. Pinning an account that carries a different feed is
   the misconfiguration this whole allowlist exists to prevent, and it is
   cheaper to reject here than to discover at the first trade.

`revoked` is written `false`. Emits `FeedQualified`.

*Re-qualification is deliberately not supported.* Changing a live feed's
parameters underneath markets already trading against it is a different
operation with different hazards; `set_feed_revoked` plus a new feed account is
the supported path.

### 3.2 `set_feed_revoked(revoked: bool)` — admin — [RECONSTRUCTED, governed by §5.7]

Flips `QualifiedFeed.revoked`. Admin-only. Emits `FeedRevocationChanged`.

**Revocation gates opening only.** It must never block `close_position`,
`admin_settle_position`, `emergency_close_position` or `cancel_withdraw`. See
§5.7: a revocation that blocked closing would convert "this feed is no longer
trustworthy" into "every position against it is now permanently trapped,
including its collateral" — turning a safety control into the largest loss
event the protocol can suffer.

The same argument is why revocation is a flag and not a close: closing the
account would let the seeds be re-initialised with different parameters, so a
position could be settled against numbers it was never opened under.

### 3.3 `create_market()` — permissionless — [RECONSTRUCTED]

Seeds `[b"market", feed.feed_id]`, `init`. Any signer pays rent. Gated on
`PauseFlags::CREATE_MARKET`.

1. `require!(!feed.revoked)`.
2. Copy `feed_id`, `price_update`, `expected_exponent`, `asset_decimals`,
   `min_price`, `max_price`, all eight guard fields and `max_divergence_bps`
   from the `QualifiedFeed`. **Copied, not referenced**, so a later feed change
   cannot retroactively alter a market's guards.
3. `market_index = exchange.num_markets`; increment with `checked_add`.
4. Every risk parameter is written **zero**. `max_oi_usd == 0` is the
   quarantine, so the market exists and cannot be traded.
5. Indices start at zero, `last_settle_ts` and `last_rate_sample_ts` at the
   current clock.

Emits `MarketCreated`. One market per feed is structural: a second call fails at
account creation, not at a check.

### 3.4 `set_risk_params(params: RiskParams)` — admin — [RECONSTRUCTED]

Admin-only. The instruction that lifts the quarantine, and the only writer of
the risk block.

Validation, all before any write:

1. `validate_margin_parameters(initial, maintenance, liquidation_fee)` — §3.10,
   already in `crates/risk`. Rejects parameters that would let a trader open at
   maximum leverage and immediately self-liquidate for the liquidator's cut.
2. `spread_bps <= MAX_SPREAD_BPS`, `open_fee_bps` and `close_fee_bps` each
   `<= MAX_TRADE_FEE_BPS`.
3. `max_profit_bps <= BPS_DENOMINATOR`.
4. `fees_dominate_funding(open_fee_bps, close_fee_bps, funding_cap_per_hour,
   min_settle_interval_seconds)` — the constraint that makes funding-farming
   unprofitable. Already in `crates/risk`; asserted here rather than left as a
   comment nobody re-checks when parameters are tuned.
5. `max_settle_window_seconds > 0` — a zero window makes accrual impossible and
   the market silently free to hold.
6. `min_position_size_base > 0`, `min_notional_usd > 0`,
   `min_collateral_usd > 0`. Dust positions cost more in rent and compute than
   they can ever pay in fees, and a zero minimum makes the OI counters
   griefable.

May be called again to retune. **Tightening is always permitted; loosening is
permitted only while `long_positions + short_positions == 0`.** An open position
snapshots its own maintenance margin, so loosening cannot hurt it — but
`max_oi_usd` and the fee schedule are read live, and lowering `max_oi_usd`
below current OI would make the invariant in §4.3 unassertable.

Emits `RiskParamsSet`.

### 3.5 `settle_market()` — permissionless, needs no oracle — [RECONSTRUCTED]

Advances `cum_borrow_index` and `cum_funding_index` to now. Callable by anyone,
because a market whose indices only advance when an interested party bothers is
a market where what you owe depends on who called last.

* `Δt = min(now - last_settle_ts, max_settle_window_seconds)`, so a long gap
  cannot accrue in a single jump. `Δt == 0` returns `Ok(())` — idempotent, not
  an error, since a keeper calling twice in a slot is normal.
* Borrow: `borrow_index_delta` with the carried remainder, and the returned
  remainder is **persisted** to `borrow_remainder_carry`. Dropping it is the
  sub-additivity bug from milestone 2 — 3600 one-second settles accruing exactly
  zero where one 3600-second settle accrued 3590.
* Funding: the rate is resampled only if `now - last_rate_sample_ts >=
  min_settle_interval_seconds`; otherwise `sampled_funding_rate_per_hour` is
  applied across the whole interval. **This throttles resampling, never
  accrual** — see §5.4.
* No oracle is read. Both indices are functions of open interest and elapsed
  time, and requiring a fresh price would make settlement fail exactly when the
  oracle is degraded, which is when accrual matters most.

Emits `MarketSettled`.

### 3.6 `open_position(params: OpenPositionParams)` — [RECONSTRUCTED]

`params`: `side`, `size_base`, `collateral_quote`, `max_execution_price` /
`min_execution_price` (slippage bound, direction depending on side).

Seeds `[b"position", market, owner]`, `init` — **never** `init_if_needed`, which
would overwrite a live position's accounting with a fresh open.

Order, and the order matters:

1. Pause check `PauseFlags::OPEN_POSITION`; `require!(!feed.revoked)`;
   `require!(!market.is_quarantined())`.
2. `settle_market` logic runs first, so the position's entry indices are current.
   Opening against stale indices donates or steals accrual from every existing
   holder.
3. Load and validate the price under **trading** guards, from the pinned
   `price_update` account (§5.1). Reject on `diverges_beyond(spot, ema,
   max_divergence_bps)`.
4. `entry_price = execution_price(side, Open, mid, confidence, spread_bps)` —
   §5.3. Never better for the trader than mid.
5. `require!` the slippage bound against `entry_price`.
6. `entry_notional_usd = notional_usd_ceil(size_base, entry_price,
   asset_decimals)` — ceil, because notional is the base for margin, OI caps and
   fees, and larger is conservative for all three.
7. Minimums: `size_base >= min_position_size_base`,
   `entry_notional_usd >= min_notional_usd`,
   `quote_to_usd_floor(collateral_quote) >= min_collateral_usd`.
8. `open_fee_usd = trade_fee(entry_notional_usd, open_fee_bps)`;
   `collateral_after_fee = collateral_quote - usd_to_quote_ceil(open_fee_usd)`,
   `checked_sub`, so a fee exceeding collateral rejects rather than wraps.
9. Initial margin: `margin_requirement(entry_notional_usd, initial_margin_bps)
   <= quote_to_usd_floor(collateral_after_fee)`.
10. `reserve_quote = usd_to_quote_ceil(profit_cap_usd(entry_notional_usd,
    max_profit_bps))` — §5.2. Ceil, because the pool must reserve at least what
    it may owe.
11. OI cap: the new `long_oi_usd` or `short_oi_usd` must not exceed
    `max_oi_usd`. Per side.
12. Utilisation: `utilization_within_cap(pool.reserved_quote + reserve_quote,
    pool.quote_deposited, pool.max_utilization_bps)` — §5.5, the same exact
    comparison withdrawal uses. **This is the reserve-time ceiling §1 refers to
    as newly reachable.**
13. Pool and market accounting per §4.1, then assert §4.3.

Emits `PositionOpened`.

### 3.7 `close_position(params: ClosePositionParams)` — [RECONSTRUCTED]

`params`: slippage bound.

1. Pause check `PauseFlags::CLOSE_POSITION`. **No revocation check** (§5.7,
   §3.2). **No quarantine check** — an admin re-quarantining a market must not
   trap the positions already in it.
2. Settle the market first, so funding and borrow are current to the second.
3. Price under **trading** guards. `exit_price = execution_price(side, Close,
   mid, confidence, spread_bps)`; slippage bound asserted.
4. `pnl = unrealized_pnl(side, size_base, entry_price, exit_price, asset_decimals)`.
5. `funding = funding_owed_signed(side, entry_notional_usd, market.cum_funding_index,
   position.entry_funding_index)`; `borrow = borrow_owed(entry_notional_usd, ...)`.
   Both on **entry** notional.
6. `equity = equity(quote_to_usd_floor(collateral_quote), pnl, funding, borrow)`.
7. `close_fee_usd = trade_fee(exit_notional, close_fee_bps)` using the
   position's **snapshotted** `close_fee_bps`.
8. `settle_close(collateral_quote, reserve_quote, equity, close_fee_usd,
   collateral_decimals)` — §3.10, which applies the profit cap to gross equity
   **before** the fee and reports `bad_debt_usd` explicitly.
9. Pool and market accounting per §4.2, then assert §4.3.
10. Transfer `net_payout_quote` to the owner. Close the position account to the
    owner.

Emits `PositionClosed`, carrying `profit_capped` and `bad_debt_usd` so a capped
payout is explicable from logs rather than only from a diff.

### 3.8 `admin_settle_position()` and `emergency_close_position()` — [RECONSTRUCTED]

Keeper liquidation is M7. M5 ships two stopgaps so a liquidatable position is
not simply unreachable in the meantime.

`admin_settle_position` — admin-only, gated on `PauseFlags::LIQUIDATE`. Prices
under **liquidation** guards (looser, per the oracle module's reasoning:
refusing to liquidate is not a safe default). Requires
`is_liquidatable(equity, notional, position.maintenance_margin_bps)` — the
position's snapshot, not the market's current value. Charges
`liquidation_fee(...)`, capped at remaining collateral, then settles as §3.7.

`emergency_close_position` — admin-only. Settles a position at oracle mid with
**no** liquidation fee and **no** liquidatability requirement, for the case where
a market must be wound down (a revoked feed, a delisting). It is the only path
that can close a position the ordinary rules would refuse, which is exactly why
it charges nothing and why its use is an event worth alerting on.

Both assert §4.3 and record bad debt rather than socialising it.

### 3.9 `cancel_withdraw()` and `set_max_aum(max_aum_quote: u64)` — [RETAINED]

`cancel_withdraw` — owner-signed, transfers escrowed shares back, closes **both**
the `WithdrawRequest` (Anchor `close = owner`) and the escrow token account
(explicit `close_account` CPI, exactly as `lp_withdraw` learned to). Not gated on
`PauseFlags`, same argument as `close_stale_escrow`. This is the documented
unrecoverable bug that `open_position` is what makes reachable.

*Implemented at `40fff01`.*

`set_max_aum` — admin. Trader losses credit `quote_deposited` (§4), so a
profitable pool grows past its cap with no setter and freezes deposits
permanently.

*Not yet implemented.*

### 3.10 New pure functions in `crates/risk` — [RETAINED]

Nine functions across `scale.rs`, `position.rs`, `pool.rs`, `funding.rs` and
`oracle.rs`, plus the `GuardsNotOrdered` error variant and the matching arm in
`programs/sakura-perps/src/oracle.rs`.

*Implemented at `7c6f62c`. See that commit and the source for the full list;
76 tests cover them.*

The one behavioural fix bundled in: `posted_slot` in the future now errors
instead of skipping the slot check entirely.

---

## 4. POOL ACCOUNTING — [RECONSTRUCTED]

The pool holds three distinct pots in one vault, and the whole of §4 exists so
they are never confused:

* `quote_deposited` — **LP equity. This is AUM.** Never `quote_vault.amount`.
* `locked_quote` — trader collateral, held on their behalf. Not LP equity, not
  withdrawable.
* `pending_protocol_fees` — owed to `fee_recipient`. Not LP equity.

`reserved_quote` is not a pot; it is a *claim against* `quote_deposited`.

### 4.1 `open_position`

```
transfer collateral_quote  trader → quote_vault

pool.locked_quote        += collateral_after_fee        (trader's, not LPs')
market.locked_quote      += collateral_after_fee

split = fee_split(open_fee_usd, exchange.protocol_fee_share_bps)
pool.pending_protocol_fees += usd_to_quote_floor(split.protocol_usd)
pool.quote_deposited       += usd_to_quote_floor(split.lp_usd)   (LP revenue)

pool.reserved_quote      += reserve_quote
market.reserved_quote    += reserve_quote

market.long_oi_usd  += entry_notional_usd    (or short_oi_usd)
market.long_positions += 1                   (or short_positions)
```

The fee split floors both parts and the remainder stays in the vault as an
un-attributed surplus, which the §4.3 inequality tolerates by construction —
it is an inequality, not an equation, precisely so dust can only ever be in the
pool's favour.

### 4.2 `close_position`

```
pool.locked_quote     -= collateral_quote     (checked_sub)
market.locked_quote   -= collateral_quote
pool.reserved_quote   -= reserve_quote        (checked_sub)
market.reserved_quote -= reserve_quote

market.long_oi_usd    -= entry_notional_usd   (checked_sub; entry, not exit)
market.long_positions -= 1

// LP equity absorbs the difference between what the trader put in and what
// they take out. A trader loss credits LPs; a trader profit debits them.
if gross_payout_quote <= collateral_quote {
    pool.quote_deposited += collateral_quote - gross_payout_quote
} else {
    pool.quote_deposited -= gross_payout_quote - collateral_quote   (checked_sub)
}

split = fee_split(close_fee_usd, exchange.protocol_fee_share_bps)
pool.pending_protocol_fees += usd_to_quote_floor(split.protocol_usd)
pool.quote_deposited       += usd_to_quote_floor(split.lp_usd)

market.cum_bad_debt_usd += bad_debt_usd       (recorded, never socialised)

transfer net_payout_quote  quote_vault → owner
```

Subtracting OI at **entry** notional is what makes the counter return to zero
when every position closes. Subtracting at exit notional would leave a residue
proportional to price movement, and the OI cap would drift out of meaning.

### 4.3 The invariants, asserted after every vault-touching instruction

**I1 — solvency.** Already asserted today:

```
quote_vault.amount >= quote_deposited + locked_quote + pending_protocol_fees
```

**I2 — the reserve is honourable.** Asserted **unconditionally**, not only on the
withdrawal path (§5.5):

```
utilization_within_cap(reserved_quote, quote_deposited, max_utilization_bps)
```

**I3 — the market slices sum.** For the market being touched, its
`locked_quote` and `reserved_quote` must never exceed the pool's. A full
cross-market sum is O(markets) and not assertable on chain; the per-market
bound is what is checkable in O(1) and catches the accounting error that
matters — a market releasing more than it reserved.

---

## 5. WHAT CHANGED AFTER REFUTATION

The original recorded seven changes forced by an adversarial pass. Two were
blockers. Titles are **[RETAINED]**; arguments are reconstructed except where
marked.

### 5.1 The `price_update` account is pinned — blocker, refuter 2 — [RECONSTRUCTED]

A feed id proves the *message* is for the right feed. It does not prove the
*account* was written by anyone trustworthy. Without pinning, a caller passes
their own `PriceUpdateV2` carrying a correctly-labelled but fabricated price,
and every guard downstream validates it happily. `QualifiedFeed.price_update`
records the exact account, `Market` copies it, and `open_position` /
`close_position` require the passed account to equal it.

*Already implemented in the account layouts at `2749aaf`.*

### 5.2 `reserved_quote` gets one definition, and it is notional-scaled — blockers, refuters 1 and 3 — [RECONSTRUCTED]

The reserve is `max_profit_bps × entry_notional`, floored into USD then ceiled
into quote, snapshotted onto the position as `reserve_quote`, and that single
number is used at open (to reserve), at close (as the payout cap), and on
release. A fraction *of collateral* was the rejected alternative: it makes the
cap depend on leverage, so two positions with identical exposure reserve
different amounts and the pool's total reserve stops corresponding to what it
can be asked to pay.

The failure mode of getting this wrong is the same one as §5.5 — under-reserve
and the last position to close cannot be paid, `checked_sub` fails, and it is
permanently unclosable.

### 5.3 Confidence and spread become a price adjustment, not only a gate — major, refuter 2 — [RECONSTRUCTED]

Gating on `max_confidence_bps` alone rejects the worst prices and then treats
everything that passes as exact. Inside the tolerance the trader still chooses
when to act, so they systematically take the favourable edge of an interval the
pool is paying for. `execution_price` therefore moves the price *against* the
trader by the confidence **and** the spread, on both legs, rounding against them
in each direction.

`Market.max_oracle_drift_bps` records the largest move the asset makes inside
`trading_max_age_seconds` — the size of the free option a trader holds by
trading on a price that may be that stale.

### 5.4 A zero-cost round trip is closed twice — major, refuter 2 — [RECONSTRUCTED]

Open and immediately close. With no elapsed time there is no funding and no
borrow, so unless something else bites, the round trip is free — and a free
round trip is an option on the oracle's next tick, exercisable repeatedly.

Closed **twice**, deliberately, because either defence alone can be configured
away:

1. **Execution price** is adverse on both legs (§5.3), so the round trip loses
   twice the spread plus twice the confidence edge even at zero fees.
2. **`open_fee_bps` and `close_fee_bps`**, both charged on notional.

And the reason `min_settle_interval_seconds` is documented as a *rate-resample*
interval only: if it throttled **accrual**, a position opened and closed inside
one interval would pay no borrow at all, which reopens the same hole from a
third direction. Accrual is continuous; only the sampled rate is stepwise.

`Position.opened_slot` makes an open-and-close within a single slot detectable
even where the clock has not advanced a whole second.

### 5.5 The utilisation ceiling becomes exact, and 100% stops being configurable — major, refuters 1 and 3 — [RETAINED]

`withdrawal_leaves_enough_reserve` compared a **floored** utilisation with `<=`.
At `max_utilization_bps = 10 000`, `utilization_bps(20_001, 20_000) =
floor(10_000.5) = 10_000 <= 10_000`, so a state with `reserved > aum` was
admitted — an overhang of up to 1 bp of AUM. The consequence is not the dust: it
is that the last position to close cannot be paid, `checked_sub` fails, and it
is permanently unclosable.

Three fixes: `utilization_within_cap` compares the true rational via `mul_wide`;
`initialize_pool` rejects `max_utilization_bps >= BPS_DENOMINATOR` (a 100%
ceiling is not a ceiling); and I2 is asserted unconditionally after every
reserve-touching instruction.

Critically, the property must be written against the exact rational and **not**
against `utilization_bps`, or it proves nothing.

*Implemented at `40fff01`.*

### 5.6 `lp_withdraw` refuses to burn shares for nothing — major, refuter 1 — [RETAINED]

`assets_for_shares(shares, total, 0)` returns `0`, not an error. Then
`flow_fee(0, bps) = 0`, `net = 0`, `require!(net >= min_amount_out)` passes for
any caller who sent `min_amount_out = 0`, `gross <= quote_deposited` passes
trivially, and the burn and the `total_shares` decrement execute against a zero
transfer. The LP's position is destroyed and the loss is a windfall to the
remaining LPs. M5 is what makes `quote_deposited` able to fall sharply, and what
makes `floor(shares × quote_deposited / total_shares) == 0` ordinary for a small
holder after a large payout.

Fix: `require!(gross > 0)` and `require!(net > 0)`, mirroring `lp_deposit`.

*Implemented at `40fff01`.*

### 5.7 Revoking a feed must never trap collateral — major, refuter 1 — [RECONSTRUCTED]

The obvious implementation of revocation gates every instruction that touches
the market. That converts a safety control into the worst loss the protocol can
suffer: the moment an admin declares a feed untrustworthy, every position
against it — and all of its collateral — becomes permanently unreachable, and
the pool's reserve is locked behind positions that can never be released.

Revocation therefore gates **opening only**. Closing, admin settlement,
emergency close and every LP path remain available. `emergency_close_position`
exists precisely so a revoked market can be wound down at all.

The same reasoning makes `revoked` a flag rather than an account close: closing
would let the seeds be re-initialised with different parameters, so a position
could be settled against numbers it was never opened under.

---

## 6. WHAT THIS RECONSTRUCTION MAY HAVE LOST

Stated plainly, because an unknown gap is worse than a known one:

* The original's §3 almost certainly carried **more per-instruction validation**
  than is reconstructed here. Where a check is missing, the failure is a missing
  `require!`, not a wrong one.
* Four of the seven refutation arguments (§5.1–5.4, §5.7) are reconstructed from
  their titles. The *conclusions* are constrained by the account layouts, which
  were implemented from the original — but the reasoning that produced them, and
  any second-order consequence the refuters found, is gone.
* Any section the original had beyond §5 — a test plan, a deployment sequence —
  is not recoverable, because the headings were never read.

**Everything tagged [RECONSTRUCTED] should be adversarially reviewed before it
is implemented, at the same intensity as the original.**
