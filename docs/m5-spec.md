# Sakura Perps — Milestone 5 Specification

### Markets and positions

Program `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y`. Anchor 1.1.2. Devnet,
pre-audit. Baseline `devnet-v0.5.0` plus the three post-v0.5.0 fixes:
`7c6f62c` (the risk layer), `40fff01` (`cancel_withdraw`, the exact utilisation
ceiling, `require!(gross > 0)`), `2749aaf` (the M5 account layouts).

> ## Provenance — read this first
>
> The original of this document was written 2026-08-01, survived a refutation
> pass, and was lost with a cleaned scratchpad before it was ever committed. The
> version committed at `a0361d2` is a **reconstruction**. Sections are tagged:
>
> * **[RETAINED]** — recovered verbatim or near-verbatim. High confidence.
> * **[RECONSTRUCTED]** — rebuilt from the implemented account layouts and the
>   surviving refutation-log headings. Never independently reviewed.
> * **[REVISED]** — rewritten in the second refutation pass, 2026-08-16.
> * **[REVISED ×2]** — rewritten in the third refutation pass, 2026-08-16, which
>   attacked the second pass's own additions and found four blockers in them.
>
> Sections 3.9, 3.10, 5.5 and 5.6, and all of §2, are **[RETAINED]**.
>
> The correlation is worth stating because it is the only calibration available:
> across two adversarial passes, **every blocker landed in [RECONSTRUCTED] or
> [REVISED] text, and no [RETAINED] section has ever needed a change.** Three
> sections are still tagged [RECONSTRUCTED] and are still unreviewed — §9.9.

---

## 1. SCOPE — [RETAINED, with §1.1 added]

M5 is the qualified feed, the market, the position, the eight instructions that
create and settle them, and the five holes that only become reachable once
positions exist.

It is **not** a keeper liquidation network (M7), not cross-margin, not partial
closes or position adds, not a fee-recipient sweep, and not a rewrite of LP
share pricing.

### 1.1 The AUM scope line — [REVISED ×2]

The committed §1 said: *"AUM stays `pool.quote_deposited` exactly as it is
today."* The second pass struck that as superseded, because its premise — that
no positions exist — is precisely what M5 removes, and the owner decided to
wire up `risk::pool::aum_usd`.

**The third pass proved that decision cannot be met in M5, and the scope line is
reinstated with its reasoning replaced.** §4.4 carries the proof. In one
sentence: the pool's liability is `Σᵢ max(0, min(pnlᵢ, reserveᵢ))`, that
function does not distribute over a sum, so no aggregate maintained on a
`Market` can compute it; the only upper bound aggregates *can* give is
`pool.reserved_quote`, and pricing shares off `reserved_quote` opens a cheaper
extraction than the one it closes.

So M5 **bounds** the mispricing instead of pricing it out:

* `max_utilization_bps` is no longer only a solvency knob. It is the exact
  ceiling on how far an LP share price can be wrong, because the mispricing is
  at most the liability, the liability is at most `reserved_quote`, and I2 caps
  `reserved_quote` at `max_utilization_bps` of tracked equity.
* M5 therefore ceilings it: `M5_MAX_UTILIZATION_BPS = 2_000`, enforced in
  §3.9.1's new `set_pool_limits`.
* `aum_usd` is **not** wired, is **not** deleted, and §4.4 names its caller: the
  per-market cached per-position mark M6 must build, behind permissionless
  liquidation.

This is a reversal of an owner decision on evidence. It needs the owner's
re-confirmation before stage 3 starts, and it is the first item in §9.

---

## 2. ACCOUNTS — [RETAINED] and already implemented

`QualifiedFeed`, `Market` and `Position` shipped at `2749aaf`. **The source is
the record**; this spec matches the layouts, not the other way round. Three
properties §3 depends on, restated because they are load-bearing:

1. **A market copies its feed's parameters; it does not reference them.** All
   fifteen oracle fields are duplicated onto `Market` at `create_market`. A
   position is therefore always settled against the numbers its market was
   created with, and re-qualifying a feed cannot retroactively change them.
2. **`max_oi_usd == 0` *is* the quarantine.** A market is born quarantined and
   `set_risk_params` is what lifts it. There is no separate flag to forget.
3. **A position snapshots what it must be judged by.** `maintenance_margin_bps`,
   `liquidation_fee_bps`, `close_fee_bps` — and, from §2.1, `spread_bps`.

### 2.1 Account changes stage 3 must make — [REVISED ×2]

Complete list. The second pass's version of this list was wrong about five of
seven fields; four of those seven no longer exist.

| Account | Field | Bytes | Why |
|---|---|---|---|
| `Position` | `spread_bps: u16` | 2 | **M6.** Read live at close, it retroactively taxes every open exit and, at `confidence + spread >= mid`, *reverts* it. |
| `Market` | `quarantined_ts: i64` | 8 | **M11.** §3.8.2's delay is measured from it. |
| `Market` | `last_good_price: u128` | 16 | **B2/M11.** The oracle-free settlement price. |
| `Market` | `last_good_price_ts: i64` | 8 | Observability; never gates anything. |
| `Pool` | — none — | 0 | §4.4. |
| `Exchange` | — none — | 0 | |

`Position._reserved` shrinks `[u8; 64] → [u8; 62]`; `Market._reserved` shrinks
`[u8; 128] → [u8; 96]`. `INIT_SPACE` and account length are unchanged.

**Layout rule:** every new field is declared *immediately before* the shrunken
`_reserved`, so Borsh's positional layout is preserved and any already-allocated
account deserializes the new fields as zero. For M5 this is belt-and-braces —
no `Market` or `Position` exists on devnet, and neither `Pool` nor `Exchange`
changes — but the rule is written down because the next milestone will need it
and `Pool` *is* live.

Also required, and specified in §3.12: roughly thirty new `PerpsError` variants
and nine events. **No new `PauseFlags` bit.** `PauseFlags::ALL` stays
`0b11_1111`; `emergency_close_position` and `refresh_market_price` are exempt by
design, matching the precedent already written into `pool.rs:833–835`.

---

## 3. INSTRUCTIONS

### 3.0 The constraint discipline — [REVISED]

The committed §3 specified eight instructions and exactly one Anchor constraint.
Anchor checks the discriminator, program ownership, and the seeds you write —
nothing else. Everything else is a sentence until someone types it.

Three of the holes that left open were individually blockers: no `has_one =
market` on `Position` (close a position opened in market A against market B); an
unconstrained `quote_vault` (assert I1 against an attacker-supplied token
account and brick the exchange for the price of one minimum position); and
unconstrained destinations on the admin paths (the admin names their own ATA).

**Rules for everything below.**

1. Every account is constrained in the `#[derive(Accounts)]` struct. A check
   that could be a constraint is never a handler `require!`.
2. Every constraint carries `@ PerpsError::…` with a variant that says what
   actually went wrong. Reaching for `MathOverflow` because the right variant
   does not exist undoes this section — §3.12 is the variant list, and it is a
   deliverable, not an afterthought.
3. Admin gating is `address = exchange.admin @ PerpsError::NotAdmin` on the
   `Signer`. Never a handler check.
4. PDAs are read with `seeds = [...], bump = account.bump`, never re-derived.
5. Space is `8 + T::INIT_SPACE`. Arithmetic is checked; `saturating_*` is banned
   in value-carrying maths. Every `RiskError` goes through
   `.map_err(crate::oracle::map_risk_error)?`. `emit!` precedes the closing
   invariant assertion. `quote_vault.reload()?` before re-reading a balance the
   same instruction changed.
6. Anything deliberately left unconstrained carries the reason in a comment, as
   `pool.rs:895–898` already does.

Where a struct below constrains a PDA using a field of that same account
(`seeds = [b"market", market.feed_id.as_ref()], bump = market.bump`), the
fallback if the Anchor version in use rejects the self-reference is an
`#[instruction(feed_id: [u8; 32])]` parameter with
`constraint = market.feed_id == feed_id`. Pick one and use it uniformly.

---

### 3.1 `qualify_feed(params: QualifyFeedParams)` — admin — [RECONSTRUCTED, constraints REVISED]

```rust
#[derive(Accounts)]
#[instruction(params: QualifyFeedParams)]
pub struct QualifyFeed<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + QualifiedFeed::INIT_SPACE,
        seeds = [b"feed", params.feed_id.as_ref()],
        bump,
    )]
    pub feed: Box<Account<'info, QualifiedFeed>>,

    /// The exact account this feed is qualified against. Recorded, not trusted:
    /// §5.1. Unconstrained here because this instruction is what *establishes*
    /// the binding — every later instruction pins against `market.price_update`.
    pub price_update: Box<Account<'info, PriceUpdateV2>>,

    pub system_program: Program<'info, System>,
}
```

Validations, all in order, all `require!` with a named variant:

1. `MIN_EXPONENT <= expected_exponent <= MAX_EXPONENT` (−18 ≤ e ≤ 0).
2. `0 < min_price < max_price`, and `asset_decimals <= 18`.
3. `0 < max_divergence_bps < BPS_DENOMINATOR` — a 100% divergence tolerance is
   not a tolerance, and §3.7's clamp needs the strict inequality.
4. `validate_guard_ordering(&trading_guards, &liquidation_guards)` — liquidation
   guards must be at least as permissive as trading guards on every axis.
   `GuardsNotOrdered` already exists.
5. The passed `price_update` currently carries `params.feed_id` and
   `exponent == params.expected_exponent`. Qualifying against an account that
   does not answer today is a configuration error, and it is free to catch here.
6. **Totality of `execution_price` — M6.**
   `liquidation_max_confidence_bps + MAX_SPREAD_BPS < BPS_DENOMINATOR`,
   raised as `ConfidenceGateTooWide`.

   Validation 6 is the whole of M6's revert half, so the arithmetic is written
   out. `validate_price` admits a price only when
   `confidence × 10_000 <= max_confidence_bps × mid`. `execution_price` computes
   `adverse = confidence + mid × spread_bps / 10_000` and returns
   `RiskError::InvalidPrice` when `adverse >= mid`. So
   `adverse <= mid × (max_confidence_bps + spread_bps) / 10_000 < mid` **iff**
   `max_confidence_bps + spread_bps < 10_000`. Checking against the
   **liquidation** gate and `MAX_SPREAD_BPS` covers both guard sets and every
   legal spread at once — and it must be the liquidation gate, because
   `validate_guard_ordering` permits it to be the wider of the two and
   `admin_settle_position` prices under it.

`revoked = false`. Re-qualification is deliberately unsupported: `init` fails on
an existing feed, and the alternative — mutating a feed markets have already
copied — would let a position be settled under numbers it was never opened
under. Emits `FeedQualified`.

---

### 3.2 `set_feed_revoked(revoked: bool)` — admin — [RECONSTRUCTED, governed by §5.7]

```rust
#[derive(Accounts)]
pub struct SetFeedRevoked<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,
    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [b"feed", feed.feed_id.as_ref()], bump = feed.bump)]
    pub feed: Box<Account<'info, QualifiedFeed>>,
}
```

Flips the flag. Emits `FeedRevocationChanged`.

**Revocation gates opening only.** `close_position`, `admin_settle_position`,
`emergency_close_position`, `refresh_market_price` and every LP path stay
available. §5.7 is the argument; §6.2 is why the first pass's version of it was
unsatisfiable.

Revocation does **not** quarantine the market and does not close positions. It
is one bit, it is reversible, and it does not touch value.

---

### 3.3 `create_market()` — permissionless — [RECONSTRUCTED, constraints REVISED]

```rust
#[derive(Accounts)]
pub struct CreateMarket<'info> {
    #[account(mut, seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,   // mut: num_markets += 1

    #[account(mut)]
    pub payer: Signer<'info>,                      // rent only; no authority

    #[account(
        seeds = [b"feed", feed.feed_id.as_ref()],
        bump = feed.bump,
        constraint = !feed.revoked @ PerpsError::FeedRevoked,
    )]
    pub feed: Box<Account<'info, QualifiedFeed>>,

    #[account(
        init,
        payer = payer,
        space = 8 + Market::INIT_SPACE,
        seeds = [b"market", feed.feed_id.as_ref()],
        bump,
    )]
    pub market: Box<Account<'info, Market>>,

    pub system_program: Program<'info, System>,
}
```

Gated on `PauseFlags::CREATE_MARKET` (`MarketCreationPaused`). Any signer pays
rent — creating a market grants nothing, because the market is born quarantined.

Copies **fifteen** feed fields by value (`feed_id`, `price_update`,
`expected_exponent`, `asset_decimals`, `min_price`, `max_price`, the four
trading-guard fields, the four liquidation-guard fields, `max_divergence_bps`).
Every risk parameter is zero, so `is_quarantined()` is true. Both indices zero,
`borrow_remainder_carry` zero, `sampled_funding_rate_per_hour` zero.
`last_settle_ts = last_rate_sample_ts = quarantined_ts = clock.unix_timestamp`.
`last_good_price = 0`. `market_index = exchange.num_markets`, then increment.

Emits `MarketCreated`.

---

### 3.4 `set_risk_params(params: RiskParams)` — admin — [REVISED ×2]

```rust
#[derive(Accounts)]
pub struct SetRiskParams<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,
    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
}
```

Not pause-gated: an admin must be able to *tighten* a market while the protocol
is paused, and quarantining (`max_oi_usd = 0`) is the tightest action there is.

**Validations. All ten run on every call, including retunes.**

1. `validate_margin_parameters(initial_margin_bps, maintenance_margin_bps,
   liquidation_fee_bps)` — initial margin must exceed maintenance plus the
   liquidation fee, or a position is liquidatable the moment it opens.
2. `spread_bps <= MAX_SPREAD_BPS` (500); `open_fee_bps`, `close_fee_bps <=
   MAX_TRADE_FEE_BPS` (500).
3. `0 < max_profit_bps <= BPS_DENOMINATOR`; `initial_margin_bps > 0`.
4. **`fees_dominate_funding(open_fee_bps, close_fee_bps, funding_cap_per_hour,
   POLICY_HOLDING_PERIOD_SECONDS)` — M8.** The fourth parameter is named
   `holding_period_seconds` in the crate, and the committed spec passed it
   `min_settle_interval_seconds`, which §5.4 and `market.rs:165–167` both
   establish is a rate-**resample** interval. A new constant,
   `POLICY_HOLDING_PERIOD_SECONDS = 3_600`, is passed instead, and **the claim
   that this makes funding-farming unprofitable is withdrawn.** What it asserts
   is exactly what it says: a round trip's fees exceed the funding accruable in
   one hour. §9.2 records the multi-hour farm as open.
5. `0 < max_settle_window_seconds <= MAX_SETTLE_WINDOW_SECONDS` (7 days).
6. `min_position_size_base > 0`, `min_notional_usd > 0`, `min_collateral_usd > 0`.
   Zero minimums make dust positions free to create and expensive to carry.
7. **Rate bounds — B5.** `borrow_rate_per_hour <= MAX_BORROW_RATE_PER_HOUR`,
   `funding_cap_per_hour <= MAX_FUNDING_RATE_PER_HOUR`, `funding_sensitivity <=
   MAX_FUNDING_SENSITIVITY`, with `MAX_BORROW_RATE_PER_HOUR =
   MAX_FUNDING_RATE_PER_HOUR = RATE_SCALE / 100` (1%/hour) and
   `MAX_FUNDING_SENSITIVITY = RATE_SCALE`.

   The ceiling is what makes `cum_borrow_index` safe, and the arithmetic is
   written out because the unbounded version was a blocker. `borrow_index_delta`
   accrues at most `rate × 10_000 / (10_000 × 3_600)` per second, i.e. `1e7 /
   3_600 ≈ 2 778` index units per second at 100% utilisation. A century of
   continuous accrual is `≈ 8.8e12`. `borrow_owed = notional × Δindex /
   RATE_SCALE`, and with notional bounded by `max_oi_usd`, the product stays
   many orders below `u128::MAX` and below `i128::MAX`, which `equity` needs.
   The unbounded version admitted `9e30`, at which ~19 hours of one-second
   settles pushes `borrow_owed` past `i128::MAX` and **every** close reverts.

8. **Reserve leverage — M3.**
   `max_profit_bps <= MAX_RESERVE_LEVERAGE × initial_margin_bps`, with
   `MAX_RESERVE_LEVERAGE = 4`.

   `reserve_quote` scales with notional while the trader posts only
   `initial_margin_bps` of it, so a dollar of collateral consumes
   `max_profit_bps / initial_margin_bps` dollars of a pool-global budget. At the
   committed defaults that ratio was 20. This bounds it at 4.

   The *per-market* half of M3 needs no field and no check: `reserved_quote` for
   a market is `Σ max_profit_bps × entry_notional / 10_000`, and OI is capped at
   `max_oi_usd`, so a market's reserve is bounded by `max_oi_usd ×
   max_profit_bps / 10_000` by construction. Setting `max_oi_usd` *is* setting
   the market's reserve budget.

9. **The staleness option is charged — M2.**
   `open_fee_bps + close_fee_bps + 2 × spread_bps >= max_oracle_drift_bps`,
   raised as `FeesDoNotDominateDrift`.

   `max_oracle_drift_bps` is the largest move the asset makes inside
   `trading_max_age_seconds` — §5.3 names it as the size of the free option a
   trader holds by trading on a price that may be that stale, and then the
   committed spec charged nothing for it. Two occurrences repo-wide, one of them
   the field declaration.

   The units line up exactly: the option is worth `max_oracle_drift_bps` of
   notional; a round trip costs `open_fee_bps + close_fee_bps` of notional plus
   `spread_bps` of price on each of two legs. Requiring the second to dominate
   the first makes exercising the option unprofitable. This is the same shape as
   validation 4, and it is what §5.4's argument was missing: §5.4 closed the
   zero-cost round trip against a *fresh* oracle and said nothing about a stale
   one.

10. **Activation bookkeeping.** If `max_oi_usd` goes non-zero from zero, clear
    `quarantined_ts = 0`. If it goes zero from non-zero, set `quarantined_ts =
    clock.unix_timestamp`. **Only on the transition** — a retune that does not
    cross the boundary must not touch it, or §3.8.2's delay restarts every time
    a fee changes.

**The retune rule — replacing the struck one.** The committed rule was
*"tightening is always permitted; loosening is permitted only while
`long_positions + short_positions == 0`"*, justified by an open-interest
invariant §4.3 does not contain. It was both over- and under-restrictive, and
its failure mode was terminal: raising `borrow_rate_per_hour` reads as
tightening (more revenue to the pool), the resulting index growth bricks every
close, and lowering it back is a "loosening" the bricked positions block forever.

**The replacement is that there is no open-position gate at all.** The gate that
mattered is validation 7's ceiling.

* Parameters a position snapshots — `maintenance_margin_bps`,
  `liquidation_fee_bps`, `close_fee_bps`, `spread_bps` — may change freely. Open
  positions are unaffected by construction. This is the whole point of §2's
  third property, extended by §2.1.
* `initial_margin_bps` and `max_profit_bps` affect only new positions: margin is
  checked at open and `reserve_quote` is snapshotted at open.
* `max_oi_usd` may be lowered below current OI. It gates new opens and nothing
  else. **§4.3 contains no open-interest invariant** — I1, I2, I3, I4 and
  nothing more — so there is nothing to make unassertable. Stated explicitly
  because the struck rule's justification claimed otherwise.
* `borrow_rate_per_hour` may be lowered at any time and raised anywhere within
  validation 7's ceiling. Raising it is not "tightening"; it is the single
  change that can make a market unclosable, and the ceiling is what makes it
  safe.

Emits `RiskParamsSet` carrying the full parameter block and the quarantine
transition.

---

### 3.5 `settle_market()` — permissionless, reads no oracle — [REVISED]

```rust
#[derive(Accounts)]
pub struct SettleMarket<'info> {
    /// Read-only. Required because borrow accrual is a function of utilisation.
    #[account(seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
}
```

No signer, no pause gate. Accrual must not be pausable: a pause that stops the
clock is a subsidy to whoever is paying.

Steps:

* **Δt, clamped on both ends — M5.** `Clock::unix_timestamp` is a
  stake-weighted vote estimate and is **not** monotonic.
  ```rust
  let now = clock.unix_timestamp;
  let raw = now.checked_sub(market.last_settle_ts).ok_or(PerpsError::MathOverflow)?;
  if raw <= 0 { return Ok(()); }                       // idempotent; never writes backwards
  let dt = (raw as u64).min(market.max_settle_window_seconds as u64);
  ```
  `raw <= 0` returns `Ok(())` and writes nothing — a keeper calling twice in a
  slot is normal, and a backwards clock revision must never become a
  ~1.8e19-second accrual through an unguarded `as u64`. The `as u64` cast is
  unreachable until positivity is proven, and **no timestamp field in this
  program is ever written backwards.**
* **Borrow.** `borrow_index_delta(market.borrow_rate_per_hour,
  utilization_bps(pool.reserved_quote, pool.quote_deposited)?, dt,
  market.borrow_remainder_carry)`. The remainder is persisted, not discarded, so
  a market settled every second accrues the same as one settled hourly.
* **Funding.** `cum_funding_index += funding_index_delta(
  market.sampled_funding_rate_per_hour, dt)`.
* **Resample, on interval only.** If `now - market.last_rate_sample_ts >=
  min_settle_interval_seconds`, recompute
  `sampled_funding_rate_per_hour = funding_rate_per_hour(long_oi_usd,
  short_oi_usd, funding_sensitivity, funding_cap_per_hour)` and set
  `last_rate_sample_ts = now`. Accrual is continuous; only the rate is stepwise
  (§5.4).
* `last_settle_ts = now`. Emits `MarketSettled`.

**No oracle is read**, and that half of the committed claim survives: requiring
a fresh price would make settlement fail exactly when the oracle is degraded,
which is when accrual matters most.

~~"Both indices are functions of open interest and elapsed time."~~
**Struck — M7, verified false.** `borrow_index_delta` takes `utilization_bps`
(`funding.rs:74–79`) and short-circuits to zero accrual when it is zero
(`:86`). The only source of utilisation is `pool.reserved_quote` against
`pool.quote_deposited`. The Pool is therefore a required account, and **borrow
is coupled across markets by design**: opening a position in one market raises
the borrow rate in every other. That is a property, not an accident, and it is
written here so nobody discovers it from a support ticket.

The same accrual routine is called internally by `open_position`,
`close_position` and `admin_settle_position` before anything is read from an
index. The standalone instruction exists so accrual does not depend on trading
activity.

---

### 3.6 `open_position(params: OpenPositionParams)` — [REVISED ×2]

```rust
#[derive(Accounts)]
pub struct OpenPosition<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,

    /// Revocation lives on the feed, and only opening reads it (§5.7). The seed
    /// derivation is the binding — no separate `has_one` is possible or needed.
    #[account(
        seeds = [b"feed", market.feed_id.as_ref()],
        bump = feed.bump,
        constraint = !feed.revoked @ PerpsError::FeedRevoked,
    )]
    pub feed: Box<Account<'info, QualifiedFeed>>,

    /// §5.1, as a constraint rather than a sentence.
    #[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
    pub price_update: Box<Account<'info, PriceUpdateV2>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init,                                   // never init_if_needed
        payer = owner,
        space = 8 + Position::INIT_SPACE,
        seeds = [b"position", market.key().as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(address = exchange.collateral_mint @ PerpsError::WrongCollateralMint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [b"quote_vault"],
        bump = pool.vault_bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_token_account.mint == exchange.collateral_mint
            @ PerpsError::WrongCollateralMint,
        constraint = owner_token_account.owner == owner.key()
            @ PerpsError::NotTokenOwner,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,

    pub system_program: Program<'info, System>,
}
```

`init`, never `init_if_needed`: seeds are `[b"position", market, owner]`, so one
position per owner per market, and `init_if_needed` would silently overwrite an
open position's entry price and indices. Adding to a position is M6
(`blended_entry_price` already exists for it, unused).

**Thirteen steps, in this order. The order is part of the specification.**

1. **Gates.** `!exchange.paused(PauseFlags::OPEN_POSITION)` (`TradingPaused`);
   feed not revoked (constraint above); `!market.is_quarantined()`
   (`MarketQuarantined`).
2. **Accrue.** Run §3.5's routine so `cum_borrow_index` and `cum_funding_index`
   are current before they are snapshotted.
3. **Price, with divergence rejected.** `load_price_and_ema(price_update,
   market.feed_id, &market.trading_guards(), clock)` → `(ValidatedPrice, ema)`.
   `require!(!diverges_beyond(p.price, ema, market.max_divergence_bps)?,
   PerpsError::PriceDiverged)`. At **open**, divergence is a reject: refusing to
   open is a safe default, and it is the only leg where that is true (§6.9).
   Write `market.last_good_price = p.price`, `last_good_price_ts = now`.
4. **Execution price.** `entry_price = execution_price(side, PriceDirection::Open,
   p.price, p.confidence, market.spread_bps)` — adverse to the trader on both
   the confidence and the spread (§5.3). Assert the caller's slippage bound
   (`params.max_entry_price` for a long, `min_entry_price` for a short) with
   `SlippageExceeded`.
5. **Notional, once.** `entry_notional_usd = notional_usd_ceil(size_base,
   entry_price, market.asset_decimals)`. **Ceil, and one number.** It is the
   basis for the fee, the margin requirement, the reserve, open interest,
   funding and borrow. Ceiling it puts every one of those in the pool's favour,
   and using a single number is what makes §4.2's entry-notional subtraction
   return the counters to zero exactly.
6. **Minimums.** `size_base >= min_position_size_base`, `entry_notional_usd >=
   min_notional_usd`, `quote_to_usd_floor(collateral_deposited_quote) >=
   min_collateral_usd`. All `PositionTooSmall`.
7. **The open fee, bound to a name.**
   ```rust
   let open_fee_usd   = trade_fee(entry_notional_usd, market.open_fee_bps)?;
   let open_fee_quote = usd_to_quote_ceil(open_fee_usd, decimals)?;
   let collateral_after_fee = collateral_deposited_quote
       .checked_sub(open_fee_quote).ok_or(PerpsError::MathOverflow)?;
   ```
   `open_fee_quote` is the amount the vault retains as fee revenue. §4.1 splits
   **that**, never `open_fee_usd`. This is B1's rule applied at the open leg.
8. **Initial margin, on what is left.**
   `quote_to_usd_floor(collateral_after_fee) >= margin_requirement(
   entry_notional_usd, market.initial_margin_bps)`, else `InsufficientMargin`.
   Checked after the fee, because a position that cannot pay its own opening fee
   and still meet margin is opening under-margined.
9. **Snapshots.** `maintenance_margin_bps`, `liquidation_fee_bps`,
   `close_fee_bps`, **`spread_bps`** (M6), `entry_borrow_index =
   market.cum_borrow_index`, `entry_funding_index = market.cum_funding_index`,
   `opened_ts`, `opened_slot`.
10. **Reserve.** `reserve_quote = usd_to_quote_ceil(profit_cap_usd(
    entry_notional_usd, market.max_profit_bps))` — §5.2. Ceil: the pool must
    reserve at least what it may owe.
11. **Open-interest cap.** The side's OI after the add must satisfy
    `<= market.max_oi_usd`, else `OpenInterestCapExceeded`.
12. **Utilisation ceiling.** After the ledger in §4.1,
    `utilization_within_cap(pool.reserved_quote, pool.quote_deposited,
    pool.max_utilization_bps)` must hold, else `UtilizationTooHigh`. This is I2,
    asserted here and not only on the withdrawal path (§5.5). The per-market
    reserve budget needs no separate check — §3.4 validation 8.
13. **Transfer, ledger, invariants, event.** `transfer_checked` the gross
    collateral into `quote_vault`; apply §4.1; `emit!(PositionOpened)`; assert
    I1–I4.

---

### 3.7 `close_position(params: ClosePositionParams)` — [REVISED ×2]

Accounts are `OpenPosition`'s, minus `system_program`, minus the `feed`
(revocation must not gate closing — §5.7), and with the position closed instead
of created:

```rust
#[account(
    mut,
    close = owner,
    has_one = owner  @ PerpsError::NotPositionOwner,
    has_one = market @ PerpsError::WrongMarket,
    seeds = [b"position", market.key().as_ref(), owner.key().as_ref()],
    bump = position.bump,
)]
pub position: Box<Account<'info, Position>>,
```

`has_one = market` is the constraint whose absence let a position opened in
market A be closed against market B. The seeds already imply it; both are
written, because the seeds constraint is the one an implementer is most likely
to "simplify" away.

**Ten steps.**

1. **Pause only.** `!exchange.paused(PauseFlags::CLOSE_POSITION)`
   (`ClosingPaused`). **No quarantine check, no revocation check.** A market
   that has stopped accepting new risk must still let existing risk out.
2. **Accrue** (§3.5's routine).
3. **Price, clamped both ways — M4.** Load under **trading** guards, then:
   ```rust
   let lo  = mul_div_floor(ema, (BPS_DENOMINATOR - d) as u128, BPS_DENOMINATOR)?;
   let hi  = mul_div_ceil (ema, (BPS_DENOMINATOR + d) as u128, BPS_DENOMINATOR)?;
   let mid = p.price.clamp(lo, hi);          // d = market.max_divergence_bps
   ```
   A **clamp, symmetric, never a reject.** Rejecting at close recreates B2 — and
   doing it during an active manipulation is precisely when the trap is most
   valuable to the manipulator. Symmetric, because an adverse-only clamp closes
   half of M4: it stops the pool *paying out* on a manipulated price and does
   nothing to stop it *charging* on one, and `admin_settle_position` makes a
   manipulated adverse price a *forced* exit with a fee attached. Clamping the
   mid into the band in both directions and then letting §5.3's adverse
   adjustment run on the clamped mid never pays out on a diverged price and
   never charges on one. Write `market.last_good_price = mid`.
4. **Execution price.** `exit_price = execution_price(side,
   PriceDirection::Close, mid, p.confidence, position.spread_bps)` — the
   **snapshot**, M6. Slippage bound asserted.
5. **PnL.** `unrealized_pnl(side, size_base, position.entry_price, exit_price,
   asset_decimals)`. `unrealized_pnl` is authoritative for settlement and is the
   only pnl function in the protocol; nothing may substitute for it.
6. **Funding and borrow, on entry notional.**
   `borrow_owed(position.entry_notional_usd, market.cum_borrow_index,
   position.entry_borrow_index)` and `funding_owed_signed(side,
   position.entry_notional_usd, market.cum_funding_index,
   position.entry_funding_index)`.
7. **Equity.** `equity(quote_to_usd_floor(position.collateral_quote), pnl,
   funding, borrow)`.
8. **Close fee, and settlement, both bound to names.**
   ```rust
   let exit_notional_usd = notional_usd_ceil(size_base, exit_price, decimals)?;
   let close_fee_usd = trade_fee(exit_notional_usd, position.close_fee_bps)?;
   let settlement = settle_close(
       position.collateral_quote as u128,
       position.reserve_quote as u128,
       equity_usd, close_fee_usd, exchange.collateral_decimals)?;
   ```
   `close_fee_usd` is the **input**. `settlement.close_fee_quote` is the
   **output**, and it is zero whenever `equity_usd <= 0` and otherwise
   `usd_to_quote_ceil(close_fee_usd).min(gross_payout_quote)`. §4.2 splits the
   output. This distinction is B1 and it is the single most important line in
   this document.
9. **Ledger** (§4.2), invariants I1–I4.
10. **Transfer and close.** `transfer_checked` `settlement.net_payout_quote` from
    the vault to `owner_token_account`; `emit!(PositionClosed { reason: Ordinary,
    profit_capped, bad_debt_usd, .. })`; the `close = owner` constraint returns
    rent. A zero net payout is legal here and is not the §5.6 case — the shares
    being destroyed are the trader's own position, which is genuinely worthless.

---

### 3.8 The two admin stopgaps — [REVISED ×2]

M5 ships no keeper liquidation. These two stand in for it until M7 and their
existence is a known weakness, not a design (§9.4).

#### 3.8.1 `admin_settle_position()` — admin, liquidation guards

Accounts: as `close_position`, plus `address = exchange.admin @ NotAdmin` on the
signer, and with the position's owner present only as a constrained payee:

```rust
/// CHECK: identity proven by `address =`; receives rent from `close = owner`.
#[account(mut, address = position.owner @ PerpsError::NotPositionOwner)]
pub owner: UncheckedAccount<'info>,

#[account(
    mut,
    constraint = owner_token_account.mint  == exchange.collateral_mint
        @ PerpsError::WrongCollateralMint,
    constraint = owner_token_account.owner == position.owner
        @ PerpsError::NotTokenOwner,
)]
pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
```

The `owner_token_account` constraint is the B4 fix that stops the admin naming
their own ATA as the payout destination.

Gated on `PauseFlags::LIQUIDATE` (`LiquidationPaused`).

Prices under **liquidation** guards — looser, per the oracle module's reasoning
that refusing to liquidate is not a safe default — with §3.7 step 3's **same
symmetric divergence clamp** applied to the liquidation-guard mid.

**It uses `position.spread_bps`, not `market.spread_bps`.** M6 applies here
verbatim and the second pass omitted it: this is the one exit path an admin
controls end to end, and `validate_guard_ordering` permits the liquidation
confidence gate to be the wider of the two, so `execution_price`'s revert is
*more* reachable here than on the ordinary close path.

**Liquidatability — M10.**
`is_liquidatable(equity_usd, current_notional_usd, position.maintenance_margin_bps)`
where `current_notional_usd = notional_usd_ceil(size_base, clamped_liquidation_mid,
asset_decimals)`. **Current, not entry.** Entry notional is right for funding,
borrow and OI, because those are charged on the exposure the trader contracted
for; the maintenance requirement is a statement about the exposure that exists
*now*, and at entry notional a short whose price has doubled carries half its
true requirement. Ties go to the pool (`equity <= requirement`). Else
`PositionNotLiquidatable`.

**The liquidation fee — B3, with the clamp the second pass omitted.**

```rust
let collateral_remaining_usd = quote_to_usd_floor(position.collateral_quote, decimals)?;
let liq_fee_usd = liquidation_fee(
    current_notional_usd, position.liquidation_fee_bps, collateral_remaining_usd)?;

// crates/risk: apply_liquidation_fee (§3.10a item 3). Ceil into quote, then
// clamp against what the close fee left. Ordering is fixed: close fee first.
let settled = apply_liquidation_fee(settlement, liq_fee_usd, decimals)?;
//   settled.liquidation_fee_quote = usd_to_quote_ceil(liq_fee_usd)
//       .min(settlement.gross_payout_quote - settlement.close_fee_quote)
//   settled.net_payout_quote = gross - close_fee_quote - liquidation_fee_quote
```

All three of `collateral_remaining_usd`, the rounding direction and the clamp
were unspecified. `liquidation_fee` caps only against collateral, and nothing
related it to the payout — so in the ordinary late-liquidation case (equity
decayed to a few dollars, fee computed on full notional) the transfer
underflowed and the position became permanently unliquidatable, on the only
liquidation path M5 ships. The clamp mirrors `settle_close`'s own, for the same
stated reason: *there is nothing else to take it from.*

The admin path must **not** transfer `settlement.net_payout_quote` — that field
is gross minus the close fee only.

Then §4.2's ledger, including B3's fee lines. Emits `PositionClosed { reason:
AdminSettled, .. }`. Bad debt is recorded in `market.cum_bad_debt_usd`, never
socialised.

#### 3.8.2 `emergency_close_position()` — admin, **no price account** — [REVISED ×2]

```rust
#[derive(Accounts)]
pub struct EmergencyClosePosition<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,
    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        mut,
        seeds = [b"market", market.feed_id.as_ref()],
        bump = market.bump,
        constraint = market.is_quarantined() @ PerpsError::MarketNotQuarantined,
    )]
    pub market: Box<Account<'info, Market>>,
    // ... position (has_one owner, has_one market, close = owner),
    //     owner (address = position.owner), owner_token_account (constrained),
    //     collateral_mint, quote_vault, token_program — as §3.8.1.
    //
    // THERE IS NO price_update ACCOUNT. That absence is the instruction.
}
```

**No pause gate.** A recovery path that a pause can disable is not a recovery
path — the codebase already argues this at `pool.rs:833–835` and `pool.rs:916–918`
for `close_stale_escrow` and `cancel_withdraw`.

**No price account.** This is B2's fix and it is not "loosen the guards".
Loosening produces an instruction that works when the oracle is degraded and
still fails when it is *absent*, and absent is what revocation, delisting and
outage all produce — nobody pushes updates to a revoked feed. Only removing the
account removes the dependency.

**Preconditions — M11.** The market must be quarantined (constraint above) and
`clock.unix_timestamp - market.quarantined_ts >= EMERGENCY_CLOSE_DELAY_SECONDS`
(86 400), else `EmergencyCloseTooSoon`. A negative age fails closed and delays
the instruction, which is tolerable: no value moves during the delay and the
clock corrects itself. Both preconditions are public and slow, so a wind-down is
announced by the chain a day before it happens.

**Settlement price — [REVISED ×2].** The second pass settled at
`position.entry_price`, so `pnl == 0` by construction. That is symmetric, and
only one direction was analysed: it denies the winner their profit *and forgives
the loser their loss*. On an unrotatable admin key (§3.9.3), that is a free put
on every position in any market the admin is willing to quarantine, and in a
legitimate wind-down it transfers the whole book's unrealised PnL from LPs to
losing traders.

Instead:

```rust
let reference = if market.last_good_price > 0 { market.last_good_price }
                else { position.entry_price };
let exit_price = execution_price(
    position.risk_side(), PriceDirection::Close,
    reference, 0, position.spread_bps)?;
```

`market.last_good_price` is written by every successful guard-passing price read
— `open_position`, `close_position`, `admin_settle_position`, and the
permissionless `refresh_market_price` of §3.11. It is read from the `Market`
account, not from an oracle, so **no price account is passed and no guard can
gate the instruction**, which is what B2 actually requires. It is economically
real in both directions, and the admin does not choose it: it is whatever the
market last transacted at, clamped into the EMA band by §3.7 step 3.

Confidence is passed as `0` because there is none to read; the spread is the
position's snapshot, so the exit is adverse to the trader exactly as an ordinary
close would be, and emergency close stops being *better* for the trader than the
normal path. `execution_price` is total here: `0 + spread_bps <= 500 <
BPS_DENOMINATOR`.

Funding and borrow accrue as normal (using the last settled indices — §3.5 runs
without an oracle, so they are current). **No liquidation fee.** The close fee is
charged, on `settle_close`'s clamp, so §4.2's ledger is uniform.

Emits `PositionClosed { reason: EmergencyClosed, .. }` — an event worth alerting
on. Bad debt recorded.

**Why `refresh_market_price` must be permissionless and unpausable:** without
it, an admin could pause `OPEN_POSITION` and `CLOSE_POSITION`, freeze
`last_good_price`, wait for the market to move, and then emergency-close at the
frozen price. With it, anyone can advance the reference at any time for the cost
of one transaction, and freezing it requires the feed itself to be dead — in
which case `last_good_price` genuinely is the last honest price there was.

---

### 3.9 `cancel_withdraw()`, `set_pool_limits()`, and two decorative fields

#### 3.9.0 `cancel_withdraw()` — [RETAINED]

Shipped at `40fff01`. Deliberately not pause-gated; closes an escrow and returns
shares. The reasoning at `pool.rs:916–918` is the precedent §3.8.2 reuses.

#### 3.9.1 `set_pool_limits(max_aum_quote: u64, max_utilization_bps: u16)` — admin — [REVISED ×2]

The committed §3.9 named `set_max_aum` and marked it *not yet implemented*. It
is widened by one parameter, because §1.1 makes `max_utilization_bps` the
protocol's bound on LP share mispricing and there is currently **no setter for
it at all** — it is written once at `initialize_pool` and the live devnet pool
is stuck with whatever it was given.

```rust
#[derive(Accounts)]
pub struct SetPoolLimits<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,
    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
}
```

Validations:

1. `0 < max_utilization_bps <= M5_MAX_UTILIZATION_BPS` (2 000), raised as
   `UtilizationCeilingTooHigh`. The upper bound is §1.1's; the strict lower
   bound is §5.5's (a 100% ceiling is not a ceiling, and a 0% ceiling makes
   every open impossible, which is what quarantine is for).
2. Lowering `max_utilization_bps` below current utilisation is **permitted**. It
   blocks new opens and new withdrawals until utilisation falls; it cannot make
   I2 unassertable, because I2 is asserted after each instruction against the
   value in force at that moment, and no already-open position is invalidated.
   Say this explicitly: the temptation is to gate it on open positions, and that
   is the mistake B5 punished.
3. `max_aum_quote` caps **tracked equity**, unchanged (`pool.rs:246–252`).

Emits `PoolLimitsChanged`.

#### 3.9.2 `min_liquidity_quote` is decorative — [REVISED]

`Pool.min_liquidity_quote` (`pool.rs:115`) is never written and never read.
`handle_initialize_pool` sets every other field and omits it; `shares_for_deposit`
uses the crate constant `MINIMUM_LIQUIDITY` (`crates/risk/src/pool.rs:33`)
instead. Its doc comment claims the live devnet pool reads `0` "which is inert"
— true, and true only because nothing reads it.

M5 does not use it. Stage 3 should either delete it back into `_reserved` or
wire it. Doing neither is the outcome this note exists to prevent.

#### 3.9.3 `pending_admin` promises a handshake that does not exist — [REVISED]

`Exchange.pending_admin` (`lib.rs:382`) is written `Pubkey::default()` once
(`lib.rs:108`) and read nowhere. The two-step admin handshake its doc comment
promises is not an instruction. Every M5 admin path asserts against
`exchange.admin` via `address =`, so this is not an M5 blocker — but **admin is
unrotatable**, and nothing in §3 may be written as though it were. §3.8.2's
reasoning depends on a single fixed admin key whose actions are publicly
attributable, which is exactly what an unrotatable admin is; that cuts both
ways, and §9.6 records it.

---

### 3.10 New pure functions in `crates/risk` — [RETAINED]

Nine functions across `scale.rs`, `position.rs`, `pool.rs`, `funding.rs` and
`oracle.rs`, plus the `GuardsNotOrdered` variant and the matching arm in
`programs/sakura-perps/src/oracle.rs`.

*Implemented at `7c6f62c`; 76 tests.* The one behavioural fix bundled in:
`posted_slot` in the future now errors instead of skipping the slot check.

### 3.10a Further additions stage 3 must make — [REVISED ×2]

Four. None changes an existing function's behaviour, so `map_risk_error`'s
exhaustive match (`oracle.rs:85–101`) is unaffected — and if a `RiskError`
variant is ever added, breaking that match is the intended behaviour.

1. **`fee_split_quote(fee_quote: u128, protocol_share_bps: u16) ->
   FeeSplitQuote`** (`crates/risk/src/position.rs`) — **B1.** Delegates to
   `fee_split`, renaming the fields `protocol_quote` / `lp_quote`. The
   arithmetic is unit-agnostic — bps of a `u128` with the remainder to the pool
   — so the delegation is exact. The wrapper exists so a reader of §4 cannot
   mistake a quote amount for a USD one. **This is the whole of B1 at the crate
   level**; the rest is §4's discipline about which number is passed.

2. **`apply_liquidation_fee(settlement: CloseSettlement, liq_fee_usd: u128,
   decimals: u8) -> Result<LiquidatedSettlement, RiskError>`** — **B3.** Applies
   `usd_to_quote_ceil` then `.min(gross_payout_quote - close_fee_quote)`, and
   returns a struct whose `gross_payout_quote`, `close_fee_quote`,
   `liquidation_fee_quote` and `net_payout_quote` re-sum by construction. One
   place, one clamp, one ordering, so §3.8.1 and §4.2 cannot disagree.

3. **`oracle::load_price_and_ema(price_update, feed_id, guards, clock) ->
   Result<(ValidatedPrice, u128)>`** (`programs/sakura-perps/src/oracle.rs`, not
   the crate) — **M4.** The committed §3.6 called `diverges_beyond(spot, ema, …)`
   while `load_price` returns no EMA, so the reference price had no source.

   **[REVISED ×2] The second pass specified a check that cannot exist**: it
   required returning `UnexpectedExponent` "if the two disagree", but
   `PriceFeedMessage` carries a **single** `exponent: i32` shared by
   `price`/`conf` and `ema_price`/`ema_conf`. There is no second exponent.
   The requirement is restated as what it can be: validate the message's one
   `exponent` against `market.expected_exponent` exactly once — which
   `validate_price` already does for the spot leg — then normalise **both**
   `price` and `ema_price` with that same exponent through `normalize_price`, so
   the two are commensurable by construction rather than by comparison.
   `ema_conf` is read and discarded: the EMA is a divergence *reference*, never
   a settlement price, and gating on its confidence would let a wide EMA band
   block trading on a tight spot price.

4. **A totality property test, not a function.** For every
   `liquidation_max_confidence_bps` that §3.1 validation 6 admits and every
   `spread_bps <= MAX_SPREAD_BPS`, `execution_price` returns `Ok` for any
   in-band price. It must be written against the **liquidation** guards, not the
   trading guards, or it proves the wrong thing (§3.8.1). **M6's revert path is
   closed by an arithmetic argument, and an arithmetic argument that is not
   property-tested is a comment.**

**Deleted from the second pass's list:** `market_unrealized_profit`. §4.4.

---

### 3.11 `refresh_market_price()` — permissionless — [REVISED ×2, new]

```rust
#[derive(Accounts)]
pub struct RefreshMarketPrice<'info> {
    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
}
```

No signer, no pause gate, no admin. Loads under **trading** guards, applies
§3.7 step 3's divergence clamp, and writes `market.last_good_price` and
`last_good_price_ts`. Touches no value, moves no tokens, and grants the caller
nothing.

It exists so that §3.8.2's settlement reference cannot be frozen by an admin
pausing the two instructions that would otherwise advance it. That is its whole
purpose, and it is two dozen lines.

Emits `MarketPriceRefreshed`.

---

### 3.12 Error variants and events stage 3 must add — [REVISED ×2]

`PerpsError` has 37 variants (`lib.rs:443–530`) and **not one** is an M5
rejection. Because §3.0's discipline is constraint-by-constraint
`@ PerpsError::…`, this list is a deliverable.

New group, `// ── Markets and positions ──`:

| Variant | Raised by |
|---|---|
| `InvalidFeedParameters` | §3.1 validations 1–3 |
| `ConfidenceGateTooWide` | §3.1 validation 6 — **M6** |
| `FeedRevoked` | §3.3, §3.6 |
| `WrongPriceUpdate` | `address = market.price_update` — **§5.1, B4** |
| `WrongMarket` | `has_one = market` — **B4** |
| `NotPositionOwner` | `has_one = owner`, `address = position.owner` — **B4** |
| `MarketCreationPaused`, `TradingPaused`, `ClosingPaused`, `LiquidationPaused` | the four `PauseFlags` reads |
| `MarketQuarantined` | §3.6 step 1 |
| `MarketNotQuarantined`, `EmergencyCloseTooSoon` | §3.8.2 — **M11** |
| `OpenInterestCapExceeded` | §3.6 step 11 |
| `ReserveLeverageTooHigh` | §3.4 validation 8 — **M3** |
| `BorrowRateTooHigh`, `FundingRateTooHigh`, `FundingSensitivityTooHigh` | §3.4 validation 7 — **B5** |
| `FeesDoNotDominateFunding` | §3.4 validation 4 — **M8** |
| `FeesDoNotDominateDrift` | §3.4 validation 9 — **M2** |
| `SettleWindowTooLong`, `InvalidRiskParameters` | §3.4 validations 3, 5, 6 |
| `PositionTooSmall` | §3.6 step 6 |
| `InsufficientMargin` | §3.6 step 8 |
| `PriceDiverged` | §3.6 step 3 — **M4 at open only** |
| `PositionNotLiquidatable` | §3.8.1 — **M10** |
| `UtilizationCeilingTooHigh` | §3.9.1 |
| `MarketSliceExceedsPool` | **I3** |
| `OpenInterestAccountingDrift` | **I4** |

The last two are invariant failures and are expected to be **unreachable in
correct operation**. They are named anyway, because the alternative is that a
genuine accounting drift surfaces as `MathOverflow` from the surrounding
`checked_*` idiom — which is precisely the outcome this section exists to
prevent. §9.10 says they need no passing test, only a construction test that
trips them deliberately.

Reused where they already say the right thing: `NotAdmin`,
`WrongCollateralMint`, `WrongShareMint`, `WrongTokenProgram`, `NotTokenOwner`,
`SlippageExceeded`, `UtilizationTooHigh`, `VaultInsolvent`, `PoolCapReached`,
`MathOverflow`, and everything `map_risk_error` produces.

**Events**, none of which exist: `FeedQualified`, `FeedRevocationChanged`,
`MarketCreated`, `RiskParamsSet`, `MarketSettled`, `MarketPriceRefreshed`,
`PositionOpened`, `PositionClosed` (carrying `profit_capped`, `bad_debt_usd`,
and a `reason` discriminating `Ordinary` / `AdminSettled` / `EmergencyClosed`),
`PoolLimitsChanged`. `emit!` before the closing invariant assertion, per house
style.

---

## 4. POOL ACCOUNTING — [REVISED ×2]

The pool holds three distinct pots in one vault, and the whole of §4 exists so
they are never confused:

* `quote_deposited` — **LP equity, tracked.** Never `quote_vault.amount`.
* `locked_quote` — trader collateral, held on their behalf. Not LP equity, not
  withdrawable.
* `pending_protocol_fees` — owed to `fee_recipient`. Not LP equity.

`reserved_quote` is not a pot; it is a *claim against* `quote_deposited`.

The committed §4 said of `quote_deposited`: *"**LP equity. This is AUM.**"* The
second pass struck the second sentence; §4.4 restores it, with a proof, and with
`max_utilization_bps` as the bound on how wrong it can be. The module doc at
`pool.rs:17–30` — AUM must never be the *vault balance*, because of the
ERC-4626 inflation attack — is untouched and remains correct.

**Naming — [REVISED ×2].** `collateral_quote` meant two different amounts in the
committed §4.1 and §4.2, which is B1's exact failure shape: a ledger line whose
correctness depends on which of two similarly-named numbers the reader has in
mind. Throughout §4:

* **`collateral_deposited_quote`** — the gross amount the trader transferred in.
* **`position.collateral_quote`** — always fully qualified — the field, which is
  net of the open fee (`position.rs:48`).

### 4.1 `open_position`

```
transfer collateral_deposited_quote     trader → quote_vault

pool.locked_quote        += collateral_after_fee        (trader's, not LPs')
market.locked_quote      += collateral_after_fee

// B1: split the amount the vault RETAINED, in quote base units — the same
// number §3.6 step 7 subtracted from the collateral. Never the USD figure it
// was derived from.
split = fee_split_quote(open_fee_quote, exchange.protocol_fee_share_bps)
pool.pending_protocol_fees += split.protocol_quote
pool.quote_deposited       += split.lp_quote            (LP revenue)

pool.reserved_quote      += position.reserve_quote
market.reserved_quote    += position.reserve_quote

market.long_oi_usd       += entry_notional_usd          (or short_oi_usd)
market.long_positions    += 1                           (or short_positions)
```

~~"The fee split floors both parts and the remainder stays in the vault as an
un-attributed surplus, which the §4.3 inequality tolerates by construction — it
is an inequality, not an equation, precisely so dust can only ever be in the
pool's favour."~~

**STRUCK — verified false.** `fee_split` floors the **protocol** share and gives
the **remainder** to the LP share (`crates/risk/src/position.rs:425–437`); the
two parts re-sum to exactly `fee_usd`, as the function's own doc comment states
(`:421–424`). And `usd_to_quote_floor` is `mul_div_floor(amount_usd, 10^d,
USD_SCALE)` with `USD_SCALE = 1_000_000` — the **identity** at six decimals. For
USDC the surplus is identically zero.

This matters beyond tidiness: the surplus paragraph was the *defence* §4.2
relied on by reference. With it gone, I1 sits exactly on the equality boundary —
which is fine, §4.3 shows the ledger balances to the unit — but there is no
slack left to absorb a mistake, and every ledger line must be exact rather than
approximately conservative.

With `open_fee_quote` split instead of `open_fee_usd`:
`collateral_after_fee + open_fee_quote == collateral_deposited_quote` exactly, so
liabilities rise by exactly the amount transferred in.

### 4.2 `close_position`, `admin_settle_position`, `emergency_close_position`

One ledger, three callers. The only difference is `liq_fee_quote`, which is zero
except on the admin-settle path.

```
pool.locked_quote      -= position.collateral_quote      (checked_sub)
market.locked_quote    -= position.collateral_quote
pool.reserved_quote    -= position.reserve_quote         (checked_sub)
market.reserved_quote  -= position.reserve_quote

market.long_oi_usd     -= position.entry_notional_usd    (checked_sub; entry, not exit)
market.long_positions  -= 1

// LP equity absorbs the difference between what the trader put in and what they
// take out. A trader loss credits LPs; a trader profit debits them.
if gross_payout_quote <= position.collateral_quote {
    pool.quote_deposited += position.collateral_quote - gross_payout_quote
} else {
    pool.quote_deposited -= gross_payout_quote - position.collateral_quote   (checked_sub)
}

// ── B1. settlement.close_fee_quote, NOT close_fee_usd. The first is what the
//        vault kept; the second is what was asked for before clamping. ───────
split = fee_split_quote(settlement.close_fee_quote, exchange.protocol_fee_share_bps)
pool.pending_protocol_fees += split.protocol_quote
pool.quote_deposited       += split.lp_quote

// ── B3. Admin-settle only; zero elsewhere. Already clamped by
//        apply_liquidation_fee (§3.10a item 2), so this is a retained amount. ─
liq = fee_split_quote(settled.liquidation_fee_quote, exchange.protocol_fee_share_bps)
pool.pending_protocol_fees += liq.protocol_quote
pool.quote_deposited       += liq.lp_quote

market.cum_bad_debt_usd += settlement.bad_debt_usd        (recorded, never socialised)

transfer net_payout_quote     quote_vault → owner_token_account
```

~~```
split = fee_split(close_fee_usd, exchange.protocol_fee_share_bps)
pool.pending_protocol_fees += usd_to_quote_floor(split.protocol_usd)
pool.quote_deposited       += usd_to_quote_floor(split.lp_usd)
```~~

**STRUCK — this was B1, and all three first-pass refuters found it
independently.** `close_fee_usd` is the *input* to `settle_close`;
`settlement.close_fee_quote` is its **clamped** output — zero whenever
`equity_usd <= 0`, otherwise `usd_to_quote_ceil(close_fee_usd).min(gross)`.
Splitting the input books fee revenue the vault never took, `D + L + P` then
exceeds `quote_vault.amount`, **I1 reverts the close**, and the position becomes
permanently unclosable — with no escape hatch, in a milestone that ships no
keeper liquidation. The worst case is the ordinary one: any position closing at
non-positive equity.

**The rule that makes this class of bug unrepresentable:** `fee_split_quote` may
only ever be applied to a quote amount the vault actually retained. Not a USD
figure, not a pre-clamp request, not anything derived from either. §3.6 step 7,
§3.7 step 8 and §3.10a item 2 exist to bind those amounts to names so §4 has
something correct to reach for.

**Open interest is subtracted at *entry* notional**, which is what makes the
counter return to zero when every position closes. Exit-notional subtraction
would leave a residue proportional to price movement and the OI cap would drift
out of meaning. I4 is the assertion that catches it.

### 4.3 The invariants, asserted after every vault-touching instruction — [REVISED ×2]

**I1 — solvency.** Already implemented as `assert_vault_solvent`
(`pool.rs:536`):

```
quote_vault.amount >= quote_deposited + locked_quote + pending_protocol_fees
```

**Verified line by line against §4.2**, because the whole of B1 was an I1
violation nobody had checked, and because the second pass's own derivation
skipped a precondition.

*Non-negativity of the payout, which the second pass assumed.*
`settle_close` gives `close_fee_quote <= gross_payout_quote`; `apply_liquidation_fee`
gives `liquidation_fee_quote <= gross_payout_quote - close_fee_quote`. Therefore
`close_fee_q + liq_fee_q <= gross`, `net_payout >= 0`, and the transfer is
representable. **This line is B3, and without it the derivation below proves
nothing.**

*Close.* Vault: `V' = V − net = V − (gross − close_fee_q − liq_fee_q)`.
Liabilities:

```
L' = L − position.collateral_quote
P' = P + prot(close_fee_q) + prot(liq_fee_q)
D' = D + (position.collateral_quote − gross) + lp(close_fee_q) + lp(liq_fee_q)
```

so `D'+L'+P' = D+L+P − gross + close_fee_q + liq_fee_q`, because
`fee_split_quote` re-sums exactly (§4.1's strike). Both sides fall by exactly
`gross − close_fee_q − liq_fee_q`. **I1 is preserved as an equality of
differences, with no dust in either direction.**

*Open.* Vault rises by `collateral_deposited_quote`. Liabilities rise by
`collateral_after_fee + open_fee_quote`, which is the same number by §3.6 step
7's `checked_sub`. Preserved identically.

The `quote_deposited -= gross − collateral` branch can only fail its
`checked_sub` if the pool's equity is smaller than a payout it underwrote. It
cannot be: `gross − collateral <= position.reserve_quote` by `settle_close`'s
cap (`position.rs:500–506`), and I2 guarantees `reserved <= max_utilization_bps
× quote_deposited / 10_000` with `max_utilization_bps < BPS_DENOMINATOR`.

**I2 — the reserve is honourable.** Asserted **unconditionally**, not only on
the withdrawal path (§5.5):

```
utilization_within_cap(reserved_quote, quote_deposited, max_utilization_bps)
```

The denominator is `quote_deposited` — tracked equity. It is not, and must never
become, an oracle-derived number: an assertion whose truth depends on a price
can be falsified by the market moving while nobody does anything wrong, and an
unconditionally-asserted falsified invariant **bricks every instruction that
asserts it, including `close_position`**. That is B2's lesson arriving from a
new direction, and it is the reason the second pass's I4 was a blocker.

**I3 — the market slices sum.** For the market being touched, its `locked_quote`
and `reserved_quote` must never exceed the pool's (`MarketSliceExceedsPool`). A
full cross-market sum is O(markets) and not assertable on chain; the per-market
bound is checkable in O(1) and catches the error that matters — a market
releasing more than it reserved.

**I4 — the position counters and open interest agree — [REVISED ×2].**

```
market.long_positions  == 0  ⟺  market.long_oi_usd  == 0
market.short_positions == 0  ⟺  market.short_oi_usd == 0
```

Asserted after `open_position` and all three settlement paths, as
`OpenInterestAccountingDrift`.

This replaces the second pass's I4 —
`usd_to_quote_ceil(max(0, marked_profit_usd)) <= pool.reserved_quote` — which
**was not an invariant.** It conflated `settle_close`'s *payout cap* with a
mark-to-market figure that no cap applies to, so an ordinary favourable price
move falsified it, and because it was asserted after every close it made
positions permanently unclosable and the pool permanently unmarkable. All three
third-pass refuters found it independently, and the section that introduced it
had argued the correct rule two paragraphs earlier for I2.

The replacement is what the old one was reaching for — the add-at-open /
subtract-at-close asymmetry — expressed as a property that is oracle-free,
cannot be falsified by the market, and is exactly true by construction.

### 4.4 LP share pricing, and why M5 does not change it — [REVISED ×2]

This section replaces the second pass's `mark_pool` design in full. It is
written as an argument rather than a specification because its conclusion is
negative and a negative conclusion has to be earned.

#### What the LP paths do

Nothing new.

```rust
// pool.rs:253–257   shares_for_deposit(net, total_shares, pool.quote_deposited)
// pool.rs:399–407   assets_for_shares(shares, total_shares, pool.quote_deposited)
```

Unchanged. `risk::pool::aum_usd` remains uncalled. `utilization_within_cap` gains
its first program callers (I2, §3.6 step 12). `MINIMUM_LIQUIDITY`, the
zero-shares rejection and the mandatory `min_shares_out` / `min_amount_out`
bounds are untouched.

#### Why: the liability is not computable from aggregates

Let `L = Σᵢ max(0, min(pnlᵢ, reserveᵢ))` — what the pool owes open positions out
of **LP equity**. The per-position cap is `settle_close`'s, which limits
`gross_payout` to `collateral_quote + reserve_quote`, so everything above the
trader's own collateral is bounded by that position's reserve.

1. **No aggregate determines `L`.** Anything a `Market` can maintain in O(1) per
   trade — `Σ size_base`, `Σ entry_notional_usd`, position counts — determines
   `Σᵢ pnlᵢ`, the book's **net exposure**. `max(0, min(·,·))` is not additive, so
   net exposure is not a liability. Two books with identical aggregates can have
   liabilities differing without bound: one long at +$300k beside one long at
   −$400k nets to −$100k, while the pool genuinely owes the winner up to its
   reserve and will recover from the loser only their collateral.

   The second pass's `market_unrealized_profit` computed exactly this netted
   figure, and its `marked_profit_usd <= 0 → 0` clamp ran on the already-netted
   total. Per-side flooring — the fix one refuter proposed — closes the
   cross-side case (a balanced book marking to zero) and leaves the within-side
   case untouched. Dispersed entry prices on one side are the normal state of a
   book, not an edge case.

2. **The only upper bound aggregates give is `pool.reserved_quote`.** Given the
   aggregates alone, `Σ_winners min(pnlᵢ, reserveᵢ)` can be anything up to
   `Σ reserveᵢ`, because a book where one position holds all the profit is
   consistent with any net exposure. So the tight bound is the reserve.

3. **Pricing shares off `reserved_quote` is worse than the bug.** Both forms
   fail.

   *Symmetric* (`AUM = D − R` for deposits and withdrawals): opening a position
   spikes `R` instantly while its PnL is still zero, so AUM drops on demand.
   Open a self-hedged pair, deposit at the depressed price, close, withdraw. On a
   $1m pool with a $400k reserve spike costing $100k of collateral, the round
   trip returns roughly $200k for roughly $7k of fees. This is §8.2's reserve
   grief with a payout attached, and it is cheaper than the extraction it was
   meant to prevent.

   *Asymmetric* (deposit at `D`, withdraw at `D − R`): the cycling attack dies,
   because the round trip is strictly lossy. But an LP who exits while
   utilisation is 40% forfeits 40% of their capital to the LPs who stay, for
   doing nothing wrong — and a large shareholder can open positions to spike `R`
   just before a queued withdrawal executes, taxing it and keeping most of the
   tax. A fee scaled to utilisation is the same number wearing a different hat
   and fails identically.

4. **Therefore any sound measure of `L` is per position.** And per-position is
   O(positions) in one instruction, which needs a hard protocol-wide position
   cap — itself a griefing surface (fill the slots) and, worse, a withdrawal
   brick the moment the true CU cost exceeds the assumed one.

**Conclusion.** M5 can have an *unsound* estimate or *no* estimate. It takes no
estimate, and the choice is recorded so it is not silently revisited.

#### What M5 ships instead: the bound

The extraction available to a first-mover LP is at most `L`; `L <=
pool.reserved_quote`; and I2 holds `reserved_quote <= max_utilization_bps /
10_000 × quote_deposited` unconditionally (§4.3). So:

> **The worst-case LP share mispricing, as a fraction of AUM, is exactly
> `max_utilization_bps`.**

That is the whole of M5's answer to M1, and it is why §3.9.1 exists and why
`M5_MAX_UTILIZATION_BPS = 2_000`. Three further facts bound it in practice, none
of which is a defence on its own: extraction requires an open position to be in
unrealised profit at the moment of exit; `withdraw_delay_seconds` and
`request_withdraw` make the exit non-atomic with the observation; and both flow
fees are charged.

`aum_usd` is retained, not deleted. Its second argument is the number M5 cannot
compute, and §9.1 names its M6 caller.

#### The M6 design this defers to, named so it is not re-invented

1. **Permissionless liquidation with a caller bounty**, replacing
   `admin_settle_position` as the primary path. Without it, losing positions run
   arbitrarily far past their collateral and unrealised bad debt — the one
   quantity that makes even a *correct* per-position mark under-state the
   liability — is unbounded.
2. **`mark_market(market, price_update, positions…)`** — permissionless, O(this
   market's positions), proving completeness from
   `long_positions + short_positions` and strictly increasing position keys,
   writing `market.marked_liability_quote` and `market.marked_at_ts`. Per
   position, so `max(0, min(pnlᵢ, reserveᵢ))` is computed where it is defined.
3. **`lp_withdraw` sums the cached per-market numbers**, using
   `min(market.marked_liability_quote, market.reserved_quote)` when a market's
   mark is fresh and **`market.reserved_quote` when it is not.** The fallback is
   the load-bearing part: a dead feed then degrades the price conservatively
   instead of freezing every LP path protocol-wide, which is what the second
   pass's all-or-nothing `mark_pool` did. Two refuters proposed this fallback
   independently and it is the right shape.
4. **`lp_deposit` keeps pricing off `pool.quote_deposited`** even then. Minting
   against the *largest* defensible AUM mints *fewer* shares, which is
   conservative for existing LPs by construction, needs no oracle, and keeps the
   recapitalisation path alive when the mark is not — the same principle
   `pool.rs:833–835` states for `cancel_withdraw`.

---

## 5. WHAT CHANGED AFTER REFUTATION (first pass, 2026-08-01)

The original recorded seven changes forced by an adversarial pass. Two were
blockers. Titles are **[RETAINED]**; arguments are reconstructed except where
marked.

### 5.1 The `price_update` account is pinned — blocker, refuter 2 — [RECONSTRUCTED]

A feed id proves the *message* is for the right feed. It does not prove the
*account* was written by anyone trustworthy. Without pinning, a caller passes
their own `PriceUpdateV2` carrying a correctly-labelled but fabricated price and
every guard downstream validates it happily. `QualifiedFeed.price_update` records
the exact account, `Market` copies it, and every price-reading instruction
requires the passed account to equal it.

**[REVISED — B4]** It was implemented in the *layouts* and nowhere else. The
requirement existed only as an English sentence; `ProbeOracle` (`lib.rs:305–313`),
the one shipped instruction taking a `PriceUpdateV2`, deliberately leaves it
unconstrained because a probe is caller-chosen. §3.6, §3.7, §3.8.1 and §3.11 now
carry `#[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]`.
A blocker closed in a spec sentence and not in a constraint is a blocker that is
still open.

### 5.2 `reserved_quote` gets one definition, and it is notional-scaled — blockers, refuters 1 and 3 — [RECONSTRUCTED]

The reserve is `max_profit_bps × entry_notional`, floored into USD then ceiled
into quote, snapshotted onto the position, and that single number is used at open
(to reserve), at close (as the payout cap) and on release. A fraction *of
collateral* was the rejected alternative: it makes the cap depend on leverage, so
two positions with identical exposure reserve different amounts and the pool's
total reserve stops corresponding to what it can be asked to pay.

*Confirmed sound by both later passes; not re-litigated. §3.4 validation 8
prices the budget it consumes rather than changing the definition.*

### 5.3 Confidence and spread become a price adjustment, not only a gate — major, refuter 2 — [RECONSTRUCTED]

Gating on `max_confidence_bps` alone rejects the worst prices and then treats
everything that passes as exact. Inside the tolerance the trader still chooses
when to act, so they systematically take the favourable edge of an interval the
pool is paying for. `execution_price` therefore moves the price *against* the
trader by the confidence **and** the spread, on both legs.

~~"`Market.max_oracle_drift_bps` records the largest move the asset makes inside
`trading_max_age_seconds` — the size of the free option a trader holds by trading
on a price that may be that stale."~~

**[REVISED — M2]** The sentence is *true*, and for the life of the document it
was also the field's only appearance outside its own declaration. Naming the size
of a free option and charging nothing for it is worse than not naming it, because
it reads as though the option has been dealt with. §3.4 validation 9 now charges
it: the round trip's fees plus two legs of spread must dominate the drift.

The alternative — an age-proportional term inside `execution_price` — is more
precise and touches the one function every value path goes through. §9.5.

### 5.4 A zero-cost round trip is closed twice — major, refuter 2 — [RECONSTRUCTED]

Open and immediately close. With no elapsed time there is no funding and no
borrow, so unless something else bites the round trip is free — and a free round
trip is an option on the oracle's next tick.

Closed **twice**, because either defence alone can be configured away: execution
price is adverse on both legs (§5.3), and both `open_fee_bps` and `close_fee_bps`
are charged on notional.

And the reason `min_settle_interval_seconds` is a *rate-resample* interval only:
if it throttled **accrual**, a position opened and closed inside one interval
would pay no borrow, reopening the hole from a third direction. Accrual is
continuous; only the sampled rate is stepwise. `Position.opened_slot` makes a
same-slot round trip detectable even where the clock has not advanced.

**[REVISED — M8]** This section survives; §3.4's *use* of it did not. Passing
`min_settle_interval_seconds` to `fees_dominate_funding` and calling the result
"the constraint that makes funding-farming unprofitable" is refuted by this
section's own insistence on what that field means. §8.4, §9.2.

**[REVISED ×2 — M2]** §5.4 closes the round trip against a *fresh* oracle. §3.4
validation 9 is the same argument against a *stale* one, and it was missing.

### 5.5 The utilisation ceiling becomes exact, and 100% stops being configurable — major, refuters 1 and 3 — [RETAINED]

`withdrawal_leaves_enough_reserve` compared a **floored** utilisation with `<=`.
At `max_utilization_bps = 10 000`, `utilization_bps(20_001, 20_000) =
floor(10_000.5) = 10_000 <= 10_000`, so a state with `reserved > aum` was
admitted. The consequence is not the dust: the last position to close cannot be
paid, `checked_sub` fails, and it is permanently unclosable.

Three fixes: `utilization_within_cap` compares the true rational via `mul_wide`;
`initialize_pool` rejects `max_utilization_bps >= BPS_DENOMINATOR`; and I2 is
asserted unconditionally. The property must be written against the exact rational
and **not** against `utilization_bps`, or it proves nothing.

*Implemented at `40fff01`. Confirmed sound by both later passes. §1.1 gives this
ceiling a second job, which is why §3.9.1 finally gives it a setter.*

### 5.6 `lp_withdraw` refuses to burn shares for nothing — major, refuter 1 — [RETAINED]

`assets_for_shares(shares, total, 0)` returns `0`, not an error. Then
`flow_fee(0, bps) = 0`, `net = 0`, `require!(net >= min_amount_out)` passes for
any caller who sent `min_amount_out = 0`, and the burn executes against a zero
transfer. The LP's position is destroyed and the loss is a windfall to the
remaining LPs. M5 is what makes `quote_deposited` able to fall sharply, and what
makes `floor(shares × quote_deposited / total_shares) == 0` ordinary for a small
holder after a large payout.

Fix: `require!(gross > 0)` and `require!(net > 0)`, mirroring `lp_deposit`.

*Implemented at `40fff01`. Confirmed sound by both later passes.*

### 5.7 Revoking a feed must never trap collateral — major, refuter 1 — [RECONSTRUCTED, argument REVISED]

The obvious implementation of revocation gates every instruction that touches the
market. That converts a safety control into the worst loss the protocol can
suffer: the moment an admin declares a feed untrustworthy, every position against
it becomes permanently unreachable and the pool's reserve is locked behind
positions that can never be released.

Revocation therefore gates **opening only**. The same reasoning makes `revoked` a
flag rather than an account close: closing would let the seeds be re-initialised
with different parameters, so a position could be settled against numbers it was
never opened under.

**[REVISED — B2] The promise was unsatisfiable as specified, and the reason
generalises.** The first pass correctly specified that *the flag* does not gate
closing, then failed to notice that all three close paths gated themselves on
something else revocation destroys: a **passing oracle read**. Nobody pushes
price updates to a revoked feed, so the instruction written to make a revoked
market closeable could not run on one.

*A recovery path must be checked against the state that creates the need for it,
not against the state in which it was written.* §3.8.2 takes no price account,
which is the only form that satisfies this section.

**[REVISED ×2]** The second pass then broke the same rule from the other side:
`mark_pool` put a mandatory oracle read on both LP paths, so a revoked feed
froze every deposit and withdrawal protocol-wide and forever. §4.4.

---

## 6. THE SECOND REFUTATION PASS (2026-08-16) — [REVISED]

Three refuters attacked the committed reconstruction. Every blocker landed in
**[RECONSTRUCTED]** text; every **[RETAINED]** section survived.

**6.1 B1 — the fee split applied to a number the vault never held.** All three
found it independently, which is itself the finding: invisible in prose, obvious
in a ledger. §4.1 (the struck defence), §4.2 (the strike and replacement),
§3.10a item 1.

**6.2 B2 — no oracle-free exit.** §3.8.2, §5.7. The fix is **not** "loosen the
guards": loosening works when the oracle is degraded and still fails when it is
absent, and absent is what revocation, delisting and outage all produce.

**6.3 B3 — the liquidation fee had no ledger line.** §3.8.1, §4.2, §4.3. The
committed sentence — "charges `liquidation_fee(...)` … then settles as §3.7" —
made every natural implementation wrong, because §3.7's settlement computes the
payout from equity, which does not contain the fee: an implementer either books
it without deducting it (breaking I1) or deducts it *and* lets it fall out of
`collateral − gross` (double-counting).

**6.4 B4 — eight instructions, one Anchor constraint.** §3.0 and the structs
throughout §3. The largest gap in the committed document, and the one most likely
to be under-estimated, because prose that says "require the passed account to
equal it" reads exactly like a constraint that does not exist.

**6.5 B5 — unbounded rates, and a retune rule that made it unrecoverable.**
§3.4 validation 7 and the replaced retune rule. `cum_borrow_index` is monotonic,
`borrow_owed` rejects a backwards index rather than crediting it, and
`settle_market` is permissionless — so the damage is reachable by anyone, is
irreversible, and the committed rule blocked the repair on the grounds that
positions were open, which the damage guaranteed forever.

**6.6 M1 — LP shares priced off tracked equity.** The second pass's answer was
`mark_pool`. §7 is what happened to it.

**6.7 M2 — `max_oracle_drift_bps` stored and never read.** §3.4 validation 9,
§5.3.

**6.8 M3 — the reserve consumes a pool-global budget the trader barely pays
for.** §3.4 validation 8; the per-market half is implied by `max_oi_usd`. Priced,
not eliminated — §9.3.

**6.9 M4 — divergence checked at open and not at close.** §3.7 step 3.
**[REVISED ×2]** The second pass specified an adverse-only clamp, which closes
half of it — it stops the pool *paying out* on a manipulated price and does
nothing to stop it *charging* on one, and `admin_settle_position` turns that into
a forced exit with a fee. The clamp is now symmetric.

**6.10 M5 — no lower clamp on Δt.** §3.5.

**6.11 M6 — `spread_bps` read live, and the revert it enabled.** §2.1, §3.1
validation 6, §3.7 step 3. **[REVISED ×2]** and §3.8.1, which the second pass
omitted — the one exit an admin controls end to end, priced under the looser
confidence gate where the revert is *more* reachable. Validation 6 is
correspondingly stated against `liquidation_max_confidence_bps`.

**6.12 M7 — "functions of open interest and elapsed time".** §3.5, struck.

**6.13 M8 — `fees_dominate_funding` credited with a constraint it does not
impose.** §3.4 validation 4; claim withdrawn; §9.2.

**6.14 M10 — `is_liquidatable` notional unspecified.** §3.8.1.

**6.15 M11 — emergency close had no adverse adjustment and no precondition.**
§3.8.2. **[REVISED ×2]** — see §7.4.

---

## 7. THE THIRD REFUTATION PASS (2026-08-16) — [REVISED ×2]

Three refuters attacked the second pass. Every blocker they found was in text
the second pass had *added*. Recorded in full, because a document whose fixes
introduce blockers needs that fact visible.

**7.1 I4 was a false invariant on a value path — blocker, all three.**
`usd_to_quote_ceil(max(0, marked_profit_usd)) <= reserved_quote` conflates
`settle_close`'s payout cap with an uncapped mark-to-market figure. An ordinary
favourable move falsifies it; `mark_pool` reverts, so the mark can never be
refreshed; and because §4.3 asserted it "after every close", every close reverts
too. Two refuters produced worked examples on a 2× move; one showed a flat,
innocent position becoming unclosable because of *another* position's profit. The
self-refutation is on the page: §4.4's `.min(reserved_quote)` clamp exists only
because the bound can be exceeded. §4.3's I4 is replaced with an oracle-free one.

**7.2 The aggregate mark reproduced M1 — blocker, all three.** §4.4. One refuter
proposed per-side flooring; the other two showed it is still wrong, because
positions net within a side. **Adjudicated in favour of the latter two**, and the
result generalised into §4.4's impossibility argument.

**7.3 The liquidation fee was unclamped and its inputs unbound — blocker, two
refuters.** §3.8.1, §3.10a item 2, §4.3's non-negativity line. B1's rule was
violated by B3's fix inside the same code block.

**7.4 Settling emergency closes at `entry_price` forgave every loser — major.**
§3.8.2, now `market.last_good_price` with an adverse execution adjustment, plus
§3.11 so the reference cannot be frozen by a pause. The second pass analysed only
the direction in which the *winner* was denied.

**7.5 The mark put an oracle on the LP paths — blocker, two refuters.** One dead
feed among sixteen froze `lp_deposit` and `lp_withdraw` permanently, with
quarantine — itself documented as mispricing shares — as the only escape.
Deleted with `mark_pool`; the fallback both refuters proposed is carried into
§4.4's M6 sketch.

**7.6 The close-side mark correction was biased and lived in the wrong
section.** §4.2 was the authoritative ledger and contained no mark line;
`admin_settle_position` was omitted from §4.4's enumeration; and the correction
subtracted at the exit price what the mark added at the mark price, accumulating
a same-signed residue the attacker could time against `lp_withdraw` in one
transaction. Moot with the mark deleted; the *lesson* — a state mutation
described only in prose is a mutation that does not get implemented — is kept.

**7.7 `active_markets` was a cached count with no repair path.** It gated all LP
flow, was maintained by edge detection inside `set_risk_params`, and had no
invariant, no recomputation and no setter. One over-count on a routine retune
froze the LP side permanently. Deleted.

**7.8 Smaller.** `PerpsError::PoolInsolvent` was unreachable under I2 plus the
reserve clamp, defended by a paragraph arguing for a branch that cannot execute
(moot). `load_price_and_ema`'s exponent cross-check was impossible — one
`exponent` field, shared (§3.10a item 3). The freshness gate reverted on clock
regression, the second pass's own M5 finding unapplied to its own new code
(moot). `collateral_quote` was overloaded across §4.1 and §4.2 (§4's naming
note). The account-change list was wrong about five of seven fields (§2.1). And
"32 accounts fit one transaction" was an account-count claim carrying a
compute-budget argument (moot).

---

## 8. CLAIMS STRUCK AS FALSE

Recorded rather than deleted, because a reconstruction's value is the record of
what was wrong. Each was verified against source before being struck.

**8.1 — §4.1, the fee-split surplus.**
> ~~"The fee split floors both parts and the remainder stays in the vault as an
> un-attributed surplus, which the §4.3 inequality tolerates by construction."~~

**False.** `fee_split` floors the protocol share and gives the remainder to the
LP share; they re-sum to exactly `fee_usd` (`position.rs:425–437`, doc comment
`:421–424`). `usd_to_quote_floor` is the identity at six decimals
(`scale.rs:94–97`, `USD_SCALE` at `:44`). For USDC the surplus is **identically
zero**, so the defence B1 rested on never existed.

**8.2 — §3.5, the nature of the indices.**
> ~~"Both indices are functions of open interest and elapsed time."~~

**False for borrow.** `borrow_index_delta` takes `utilization_bps`
(`funding.rs:74–79`) and short-circuits to zero when it is zero (`:86`).
Utilisation comes only from `pool.reserved_quote` against `pool.quote_deposited`.
The *oracle-free* half survives.

**8.3 — §3.4, the retune rule's justification.**
> ~~"lowering `max_oi_usd` below current OI would make the invariant in §4.3
> unassertable."~~

**False: §4.3 contains no open-interest invariant.** I1, I2, I3 and (now) I4,
and nothing else. The rule it justified is struck with it, on the further grounds
of being both under- and over-restrictive.

**8.4 — §3.4, `fees_dominate_funding`.**
> ~~"the constraint that makes funding-farming unprofitable"~~

**False.** The fourth parameter is `holding_period_seconds`
(`funding.rs:236`) and it was passed `min_settle_interval_seconds`, which §5.4
and `market.rs:165–167` both establish is a rate-**resample** interval.

**8.5 — §3.8, emergency close at oracle mid.**
> ~~"Settles a position at oracle mid"~~

**Unsatisfiable.** A revoked or delisted feed publishes no updates, so the mid
does not exist in the circumstance the instruction exists for. It is also
*better for the trader* than `execution_price` on either leg, compounding M11.

**8.6 — §4, `quote_deposited`.**
> ~~"**LP equity. This is AUM.**"~~

**[REVISED ×2] Un-struck, with its bound made explicit.** The second pass struck
this on the strength of a replacement that does not work (§4.4). `quote_deposited`
is tracked equity and M5 prices shares off it; the difference from true AUM is
bounded by `max_utilization_bps`, which §3.9.1 ceilings. The adjacent argument
that AUM must never be `quote_vault.amount` (`pool.rs:17–30`) was never in
question.

**8.7 — §4.3, the second pass's I4.**
> ~~"`usd_to_quote_ceil(max(0, pool.marked_profit_usd)) <= pool.reserved_quote`.
> The pool can never owe a position more than its `reserve_quote`, so the sum of
> positive marks is bounded by the sum of reserves."~~

**False.** `settle_close` caps the *payout*; nothing caps the *mark*. Falsified
by ordinary price movement, and asserted on paths whose reversion is terminal.

**8.8 — §3.11, the aggregate identity as a liability.**
> ~~"`Σᵢ pnlᵢ = notional(long_size_base, P) − long_oi_usd`"~~

**True as algebra, false as a liability.** It computes net exposure. §4.4.

**8.9 — §3.11, the account-change claim.**
> ~~"The two new `*_size_base` aggregates are the only thing missing today."~~

**False.** Seven fields were new, not two, and `Market.quarantined_ts` — which
§3.8.2's precondition measures from — was named nowhere in any list.

**8.10 — §3.10a, the EMA exponent check.**
> ~~"returns `RiskError::UnexpectedExponent` if the two disagree"~~

**Impossible.** `PriceFeedMessage` carries one `exponent`, shared by spot and
EMA. §3.10a item 3.

---

## 9. WHAT REMAINS UNCLOSED

Stated plainly, because an unknown gap is worse than a known one.

### 9.1 M1 is bounded, not closed — and this reverses an owner decision

M5 prices LP shares off `pool.quote_deposited`. A departing LP can be over-paid
by up to the pool's true liability to open positions, which is at most
`max_utilization_bps` of tracked equity — 20% at M5's ceiling, and equal to
current utilisation in practice.

§4.4 proves no aggregate can do better and that the reserve bound is worse than
the bug. The fix is M6: permissionless liquidation, per-market cached
per-position marks with a `reserved_quote` fallback for unmarkable markets, and
`lp_deposit` left on tracked equity.

**Confidence: high that the impossibility argument is correct; high that the
bound is right; the judgement that 2 000 bps is an acceptable ceiling is a
judgement.** This item requires the owner's explicit re-confirmation before
stage 3 begins — it is the one place this document overrides a decision that was
already made.

### 9.2 The multi-hour funding farm — open, and no longer misdescribed

§3.4 validation 4 asserts that a round trip's fees exceed funding accruable in
one hour. It says nothing about holding the light side of a skewed market for six
hours or a day. `funding_cap_per_hour` bounds the rate; nothing bounds the
integral. The honest closers, none of which M5 ships: a minimum holding period
before funding accrues to the receiving side; funding that decays with position
age; or a cap derived from the fee schedule over a *policy* horizon. The first is
smallest and is the natural M6 item. **Confidence: high. The mechanism is
arithmetic, not speculative.**

### 9.3 The reserve grief is priced, not eliminated

§3.4 validation 8 caps reserve leverage at 4× and `max_oi_usd` bounds the
per-market reserve. A self-hedged pair still consumes the pool-global budget; it
now costs a quarter of it in real collateral instead of a twentieth, and pays
borrow continuously. Not eliminable while the reserve is notional-scaled, and
§5.2 settles that it must be. The eventual answer — a reserve that shrinks as a
position's profit cap becomes unreachable — requires per-position marking, i.e.
§9.1's M6 work. **Confidence: high on the bound; medium on whether 4× is right.
`MAX_RESERVE_LEVERAGE` is a judgement, not a derivation.**

### 9.4 There is no keeper liquidation, and that is now load-bearing in two places

`admin_settle_position` is the only liquidation path and it runs at whatever pace
an admin runs it. Positions therefore go arbitrarily far past their collateral
and `cum_bad_debt_usd` accumulates unbounded. This is a known M5 limitation, but
two things now depend on it that did not before: B3's clamp is reachable
routinely because late liquidations are the normal case, and §4.4's M6 design
cannot be trusted without it, because unrealised bad debt makes even a correct
per-position mark under-state the liability. **Confidence: high. Ship M6's
permissionless liquidation before M6's mark.**

### 9.5 The staleness option is charged, but not proportionally to age

§3.4 validation 9 charges the *worst-case* drift over the full trading age on
every trade. A trader acting on a one-second-old price pays the same as one
acting on a fifty-nine-second-old price, and the latter still holds a free option
the former does not. The precise fix is an age-proportional term inside
`execution_price`: `mid × max_oracle_drift_bps × age / (trading_max_age_seconds
× 10_000)`, adverse on both legs. Rejected for M5 because it modifies the one
function every value path goes through, and because the guards already bound
`age` tightly (60 seconds against 16–24 observed on the live devnet SOL/USD
feed). **Confidence: high that it is the better long-run design; medium that it
matters at M5's guard settings.**

### 9.6 The admin is unrotatable

`pending_admin` is decorative (§3.9.3). Every M5 admin path — `qualify_feed`,
`set_feed_revoked`, `set_risk_params`, `admin_settle_position`,
`emergency_close_position`, `set_pool_limits` — asserts against a key that cannot
be changed and cannot be recovered. §3.8.2's reasoning explicitly relies on the
admin being a single publicly-attributable key; it does not survive that key
being compromised. A two-step handshake is small and is not in M5.

### 9.7 Emergency close still moves value at a price nobody chose

`market.last_good_price` is better than `entry_price` in both directions and
removes the admin's timing choice, but it can be arbitrarily stale — that is the
point, and it is also the residue. A market whose feed died a week before the
wind-down settles at a week-old price, and whoever that favours, it is not a
price either party agreed to. The bound is quarantine plus a day of public
notice, the adverse spread adjustment, and `refresh_market_price` being
permissionless and unpausable. **Confidence: medium on severity. It is the least
bad of the available oracle-free settlements, not a good one.**

### 9.8 Where the refuters disagreed, and how it was adjudicated

* **Per-side flooring of the aggregate mark.** Refuter 1 proposed it as the fix
  for the netting blocker; refuters 2 and 3 showed it leaves within-side netting
  untouched. **Adjudicated for 2 and 3**, and generalised into §4.4's argument
  that no aggregate works at all.
* **`liability = pool.reserved_quote`.** Refuter 3 proposed it as the fix and
  refuter 2 offered it as an option, both describing it as strictly safe.
  **Rejected**, with the cycling and confiscation arithmetic in §4.4 point 3.
  This is a case where a refuter's proposed fix opens a cheaper extraction than
  the bug it closes, and it is recorded because the temptation to reach for it
  will recur.
* **Whether `emergency_close_position` should settle at `entry_price` or a
  stored last-good price.** Only one refuter raised the loser-forgiveness half.
  **Adopted**, at the cost of three new `Market` fields and one new instruction —
  the largest addition in this pass, and the one most deserving of a fourth
  refutation.
* **Substituting a market's `reserved_quote` when its price is unavailable.**
  Two refuters proposed it independently as the fix for the dead-feed freeze.
  **Adopted into the M6 design** in §4.4, moot for M5.
* **`lp_deposit` off the mark entirely.** One refuter; **adopted**, and now
  trivially satisfied.

### 9.9 Still tagged [RECONSTRUCTED], and therefore still unreviewed

Exactly three sections: **§5.1**, **§5.2** and **§5.4**. Each survived both later
passes — §5.2 and §5.4 confirmed sound, §5.1's conclusion confirmed while its
*implementation* was found missing — but none has been independently re-derived.
They are conclusions constrained by shipped account layouts with reconstructed
reasoning behind them.

The correlation across three passes is the best calibration available: §5.5 and
§5.6, the two [RETAINED] arguments, have needed nothing; §5.1, §5.3, §5.4 and
§5.7 all needed revision; and every blocker in every pass landed in text that was
reconstructed or newly written. **Anything still tagged [RECONSTRUCTED] should be
adversarially reviewed before stage 3 implements against it.**

### 9.10 There is still no test plan

M5 ships nine instructions, three new crate functions, roughly thirty error
variants and four invariants. The committed document said no test plan survived
and this revision has not written one. Five tests are not optional, named here so
their absence is a decision rather than an oversight:

1. **I1 as a ledger property over §4.1 and §4.2**, generated across the full
   parameter space including `equity <= 0`, `profit_capped`, and both fee splits
   on the admin path. B1 and B3 were both I1 violations found by reading, not by
   testing.
2. **`apply_liquidation_fee` non-negativity**, at `gross == 0`, at
   `close_fee_quote == gross`, and where the raw fee exceeds notional.
3. **§3.1 validation 6 as a totality proof for `execution_price`**, against the
   **liquidation** guards (§3.10a item 4).
4. **§3.5's Δt handling under a backwards clock**, asserting that no timestamp is
   written and no accrual occurs.
5. **I4 as a construction test** — deliberately desynchronise `long_positions`
   from `long_oi_usd` and assert `OpenInterestAccountingDrift`, since it is
   unreachable in correct operation and would otherwise never execute.

### 9.11 No compute budget has been measured

`close_position` runs an accrual pass, a guarded oracle read with an EMA, a
divergence clamp, `execution_price`, `unrealized_pnl`, two index lookups,
`settle_close`, a nine-line ledger, four invariants and two token CPIs. Nothing
in this document establishes it fits in one transaction. The second pass made a
CU claim as an account-count claim and it was caught; this pass makes no claim at
all, which is honest and is still a gap. Measure it in stage 3 before the
instruction set is frozen.

---

*End of specification. Stage 3 writes the code.*
```

---
