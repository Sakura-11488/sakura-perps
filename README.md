# Sakura Perps

> ### ⚠️ Devnet only. Unaudited. Not for real funds.
>
> This program has not been audited, has never held anything of value, and is
> incomplete. Do not deploy it to mainnet. Do not send it money.

Permissionless oracle-and-pool perpetual futures on Solana.

**Status:** milestone 5 of 10 — markets and positions are live on devnet
(`devnet-v0.6.0`). Traders can open and close leveraged positions against the
pool. Funding, borrow fees, liquidation and the keeper are **not built**; there
is no keeper liquidation at all, so exits currently depend on the position owner
or the admin. See [What milestone 5 does not settle](#what-milestone-5-does-not-settle).

---

## What this is

Traders open leveraged long or short positions against a shared liquidity pool
at an oracle-determined price. Liquidity providers deposit collateral, receive a
share token, and take the other side of every trade in exchange for fees. This is
the structure Jupiter Perps and GMX use.

Market listing is **permissionless**: anyone may create a market for an asset
whose price feed has been qualified. A newly created market opens *quarantined*,
with zero open-interest allowance, until its risk parameters are set.

That split is deliberate and it is the core safety idea here. Permissionless
*listing* is safe. Permissionless *feed qualification* is not — on some oracle
networks anyone can create a feed backed by arbitrary jobs, so "any feed with low
variance" would be equivalent to no gate at all. The oracle allowlist is
admin-controlled; everything downstream of it is open.

## What this is not

- **Not an order book.** There are no limit orders, no makers and takers, no
  depth. Pricing comes from an oracle.
- **Not cross-margin.** Positions are isolated.
- **Not multi-market yet.** One market to begin with.
- **Not audited, and not finished.** See [Roadmap](#roadmap).

## Collateral

**USDC**, not SAKURA — a deliberate reversal of the original intent, for two
measurable reasons:

1. Pyth publishes **no price feed for SAKURA**. Without a USD price there is no
   margin ratio, and without a margin ratio there is no way to decide who is
   solvent.
2. SAKURA has **one DEX pool holding about $52,000**. Moving its price by 2×
   requires roughly 141 SOL of capital — which is flash-loanable, so the real
   *cost* is only the swap fee on both legs, on the order of **$65**. Any oracle
   derived from that pool, however well-engineered, is a $65 oracle. Total depth
   is also smaller than a single meaningful position, so liquidations could not
   clear at any price.

SAKURA instead earns fee discounts for holders and boosted yield for liquidity
providers who stake it — roles that require no price feed at all. This is the
same split GMX uses between GMX and GLP: the volatile token is never what backs
the book.

Every cluster-varying address — the collateral mint, its token program, the fee
recipient — is stored in an account, never a compile-time constant.

## Toolchain

Versions must match. If yours differ, expect failure.

| Component | Version |
|---|---|
| Anchor CLI | 1.1.2 |
| `anchor-lang` / `anchor-spl` | 1.1.2 |
| Agave CLI | 4.1.2 |
| Host Rust | 1.89.0 |
| SBF platform-tools | v1.54 (bundled with Agave 4.1.2) |
| Node | 22 |

These versions are not arbitrary — they are the only combination that works, and
finding it cost real time:

- Anchor 1.1.2 requires Rust **1.89**, via its `wincode` dependency.
- Several crates in `anchor-spl`'s transitive tree now require **edition 2024**,
  which Rust stabilised in 1.85. Older platform-tools ship Rust 1.84 and die with
  ``feature `edition2024` is required`` before compiling a line of your code.
- Agave 4.1.2 bundles platform-tools **v1.54**, whose Rust is 1.89. Older Agave
  bundles v1.51 and does not work with current Anchor.
- Installing v1.54 *alongside* an older Agave does not help: `anchor build` does
  not reach for it, and passing `--tools-version` through `anchor build --` is
  rejected by the IDL builder *after* the program has already compiled.

The Anchor CLI version **must equal** `anchor-lang`. If they drift, account
discriminators and the generated IDL diverge from what the deployed program
expects, and the failure appears at runtime rather than at build time.

The host toolchain and the SBF toolchain are **independent**. `rust-toolchain.toml`
pins the host compiler for `cargo test`, clippy and IDL generation.
`cargo-build-sbf` ships its own SBF toolchain and ignores that file entirely.

### If `anchor build` fails with ``no such command: `+<toolchain>` ``

Your `cargo` is not the rustup shim. `cargo-build-sbf` invokes
`cargo +<toolchain> build`, and the `+toolchain` syntax is a rustup feature — a
standalone Rust installation reads it as a command name and fails. Note that
`cargo-build-sbf --version` succeeds anyway, so a version check is not proof the
build will work.

Put rustup's shim first:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
which cargo   # must be ~/.cargo/bin/cargo, not a standalone install
```

## Quickstart

```bash
git clone https://github.com/Sakura-11488/sakura-perps.git
cd sakura-perps
pnpm install
anchor build
anchor test
```

## Devnet

| | |
|---|---|
| Cluster | devnet |
| Program id | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` |
| Collateral (planned) | Circle USDC-devnet `4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU` |
| Oracle receiver | Pyth `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` |
| SOL/USD feed | `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` |

Note that USDC-devnet is owned by the **legacy** SPL Token program while SAKURA
is Token-2022. The program uses `anchor_spl::token_interface` throughout so it
can accept either, and pins whichever it was initialized with.

The plan previously named `Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr`. That is
a legacy community "USDC-Dev" token whose **mint authority is the mint address
itself**, so nothing can ever mint it and no faucet for it exists or can exist.
Circle's devnet USDC is `4zMMC9sr…`, which is what faucet.circle.com dispenses.

> **Resolved since `devnet-v0.4.0`: freezable collateral is an explicit opt-in.**
> `initialize_exchange` still refuses a collateral mint carrying a freeze
> authority *by default*, which used to make USDC unusable on either cluster —
> Circle's devnet USDC has one, and so does mainnet USDC
> (`EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`, authority `7dGbd2QZ…`).
> `InitializeExchangeParams::allow_freezable_collateral` now lets the admin accept
> it deliberately, once, with the authority recorded on the `Exchange`
> (`lib.rs:99`). The default stays closed: a freeze authority is a real power over
> the vault, so taking it on is a decision someone makes on purpose rather than a
> default nobody read. The M4 pool round-trip was run against Circle's devnet USDC
> on this path.

## Deployments

Source-to-bytecode traceability. Every deploy gets a row.

| Cluster | Program id | `.so` sha256 | Tag | Date | Upgrade authority |
|---|---|---|---|---|---|
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `cb1fee92…f717289c` | `devnet-v0.1.0` | 2026-07-27 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `569798fe…0e324d47` | `devnet-v0.2.0` | 2026-07-27 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `7c833439…310bbab3` | `devnet-v0.3.0` | 2026-07-28 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `5d879e58…07e61d2d` | `devnet-v0.4.0` | 2026-07-28 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `6abbbf6d…fdba2a87` | `devnet-v0.5.0` | 2026-07-28 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `668dc905…fe052a80` | `devnet-v0.6.0` | 2026-08-26 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `fcc3a2ba…e6973a1e` | `devnet-v0.7.0` | 2026-08-26 | `5JSAncTb…dKP` |

### Pool round-trip, verified on devnet

`devnet-v0.4.0` against Circle's devnet USDC
(`4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU`), run twice by
`tests/devnet-pool-roundtrip.ts`:

| | Run 1 | Run 2 |
|---|---|---|
| `lp_deposit` 2 USDC | 2,000,000 shares | 2,000,000 shares |
| `request_withdraw` | ok | **ok** |
| `lp_withdraw` | ok | ok |
| LP balance after | 2 USDC | 2 USDC |
| vault after | 18 USDC (unchanged) | 18 USDC (unchanged) |

**Run 2 is the point.** It is the same liquidity provider's *second* withdrawal
from the same pool. Before the escrow fix that call failed outright — see below.

The vault keeps 18 USDC because the pool is shared and other deposits remain in
it, including the `MINIMUM_LIQUIDITY` locked forever by the first-ever deposit.
The invariant asserted is that a complete cycle returns the vault to exactly
where it started, not that it empties.

### The escrow bug this found

`request_withdraw` creates a `WithdrawRequest` **and** an escrow token account at
`[b"withdraw_escrow", owner]`. `lp_withdraw` closed only the first, so the escrow
survived and the owner's next `request_withdraw` failed at account creation
("already in use") — with no instruction in the program able to clear it. A
provider could withdraw exactly once, ever, and the rest of their position was
stranded.

The whole SVM suite was green throughout: every test stopped after a single
deposit-request-withdraw cycle, so nothing ever attempted a second. The devnet
round-trip found it on its second run. `a_provider_can_withdraw_twice` now covers
it, and asserts the escrow account is actually gone between cycles rather than
inferring it from the next call succeeding.

### `close_stale_escrow`, and the recovery it performed

`lp_withdraw` closes the escrow now, so no new orphans appear — but the ones
already on chain needed a way out, because nothing in the program could close an
escrow except `lp_withdraw`, unreachable without `request_withdraw` succeeding
first. `close_stale_escrow` is that path. Owner-signed, and narrow by design: the
escrow is addressed by PDA seeds so an owner can only reach their own, and it
refuses both a non-empty escrow (those shares belong to a live request) and one
whose `WithdrawRequest` is still open (closing it would strand that request).
Not gated on `PauseFlags` — a recovery path a pause can disable is not one.

Verified against the real casualty rather than a fixture. The admin wallet
`5JSAncTb…dKP` had been unable to withdraw since the pre-fix run:

| Step | Result |
|---|---|
| `close_stale_escrow` on `GesUd6xF…WtED` | escrow closed, 0.002034 SOL rent returned |
| `request_withdraw` 15,000,000 shares | **succeeded** — the call that had been failing |
| `lp_withdraw` | 15 USDC returned, 0 shares left |

`tests/devnet-close-stale-escrow.ts` and `tests/devnet-withdraw-position.ts`
reproduce both halves.

**Resolved: `cancel_withdraw`.** A withdrawal request that then cannot complete
— utilisation rising above `max_utilization_bps` is enough, and a trader can
cause it — used to strand the shares in escrow with no way back.
`close_stale_escrow` correctly refuses that case, since the escrow is non-empty
and the request is live, so it needed a separate instruction rather than a
loosened guard. `cancel_withdraw` (`lib.rs:229`) returns the escrowed shares and
closes the request.

### Upgrading needs `program extend` first

Every milestone so far has outgrown its ProgramData allocation, and
`solana program deploy` fails on size before writing anything. Extend first,
then deploy. The allocation has gone 183,024 → 524,288 (M4) → 767,472 (M5) →
**799,704** (permissionless liquidation).

**Size the extend from the CI artifact, never from a local `cargo-build-sbf`
output.** The two differ — the local build of the same commit came out 716,528
bytes against the artifact's 799,704 — so a local figure will tell you no extend
is needed when one is.

```bash
# M5: 524,288 -> 767,472, the exact size of the new .so
solana program extend <program-id> 243184 --url "$RPC" -k <deployer>
solana program deploy target/deploy/sakura_perps.so \
  --program-id <program-id> --upgrade-authority <deployer> -k <deployer> --url "$RPC"
```

Two things that cost time here. Pass the program **address** to `--program-id`,
not a keypair path containing spaces — the CLI rejects that as an "unrecognized
signer source", and for an upgrade the address is what it wants anyway because
the upgrade authority is the signer. And `solana program dump` returns the whole
allocation, so the file is padded with zeros to the full allocation; truncate to
the `.so` length before comparing hashes or it will never match:

```bash
solana program dump <program-id> onchain.so --url "$RPC"
head -c 767472 onchain.so | sha256sum   # == the artifact's sha256
```

Every deploy since `devnet-v0.3.0` has been verified that way. For
`devnet-v0.6.0` the on-chain bytes are byte-identical to the CI artifact from
run 32921524835 (`c058f40`), sha256 `668dc905…fe052a80`.

**Do not deploy through `api.devnet.solana.com`.** It has now failed the bulk
writes four times on this program, most recently with `Max retries exceeded`;
`--use-rpc` does not rescue it. Helius completed the identical upload first try.
Upload the buffer under a keypair **file** —
`solana program write-buffer <so> --buffer <file>` — so a partial upload resumes
instead of restarting and re-paying rent. A stranded buffer is reclaimable via
its *authority* (`solana program close <addr>`), so funds are never at risk even
if that file is lost.

### Oracle validation, verified on devnet

`devnet-v0.2.0` was checked against the live sponsored Pyth SOL/USD feed
(`7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`) in both directions:

| Guards | Result |
|---|---|
| normal — 60 s, 100 bps | **accepted**, $75.5164, confidence 3.6 bps, slot age 5 |
| `max_age` 1 s | rejected |
| `max_confidence` 1 bps | rejected |
| `expected_exponent` −6 | rejected |
| sanity band $1000–2000 | rejected |
| wrong `feed_id` | rejected |

The rejections are the point. A guard that only ever passes is indistinguishable
from no guard at all.

The deployed binary is the artifact CI produced, not a local build. `solana
program dump` returns bytes whose sha256 matches the CI artifact exactly, so the
source in this repository provably corresponds to the bytecode on chain.

ProgramData lives at `Cy3X6ZFgt1ELXh4Qnp9NcNrUKRT7U4vQrWq5SFVAdREw`.

Upgrade authority is a single key while on devnet. Before any mainnet
consideration it moves to a Squads multisig behind a timelock, and there will
never be an instruction permitting the admin to move funds out of the vault.

### Deploying

The public `api.devnet.solana.com` endpoint is unreliable for program deploys —
three attempts failed there with "30 write transactions failed", a TPU client
that could not reach the websocket, and then HTTP 429. Use a dedicated RPC:

```bash
solana program deploy target/deploy/sakura_perps.so \
  --program-id target/deploy/sakura_perps-keypair.json \
  --url "https://devnet.helius-rpc.com/?api-key=YOUR_KEY" \
  --with-compute-unit-price 50000 --max-sign-attempts 100
```

Every failed deploy strands a buffer account holding roughly 1 SOL. Reclaim them
or you will wonder where your devnet balance went:

```bash
solana program show --buffers --url "$RPC"
solana program close --buffers --url "$RPC"
```

## Testing

| Layer | Tool | Runs |
|---|---|---|
| Pure math | `cargo test` + `proptest` | every push |
| Program logic | LiteSVM | every push |
| End-to-end | `anchor test` | PRs and nightly |
| Adversarial | `trident`, `cargo-fuzz` | nightly |

LiteSVM matters more than it might appear: it allows writing account data
directly, which is how oracle staleness, wide confidence intervals, and
liquidation cascades get tested deterministically in milliseconds instead of
waiting for a real price to move.

## Milestone 5 — markets and positions

Deployed as `devnet-v0.6.0` (2026-08-26) from CI run `32921524835`. On-chain
bytes verified byte-identical to the CI artifact, `668dc905…fe052a80`.

ProgramData needed extending 524,288 → 767,472 bytes first (~1.69 SOL,
permanent). Public devnet failed the bulk writes again with `Max retries
exceeded` — the fourth such failure — and Helius completed the same upload on
the first attempt. Deploy via a dedicated RPC, not `api.devnet.solana.com`.

| Stage | What | State |
|---|---|---|
| 1 | `crates/risk` pure-function layer | done — `7c6f62c` |
| 1b | the three holes markets make reachable | done — `40fff01` |
| 2 | `QualifiedFeed`, `Market`, `Position` layouts | done — `2749aaf` |
| 3 | the eleven instructions | done — `f6d34ee` |
| 4 | LiteSVM coverage, then deploy | done — deployed `devnet-v0.6.0` |

Eleven entry points **added by M5**, bringing the program to 20 in total:
`qualify_feed`, `set_feed_revoked`, `create_market`,
`set_risk_params`, `set_pool_limits`, `settle_market`, `refresh_market_price`,
`open_position`, `close_position`, `admin_settle_position`,
`emergency_close_position`.

### The spec, and why it is written the way it is

[`docs/m5-spec.md`](docs/m5-spec.md) is authoritative and carries provenance tags
per section — `[RETAINED]`, `[RECONSTRUCTED]`, `[REVISED]`, `[REVISED ×2]`. The
original was lost with a cleaned scratchpad before it was ever committed; what is
there now is a reconstruction that has since been through three adversarial
refutation passes. Where it strikes an earlier claim as false, that record is
deliberate. The tags earned their keep: **every blocker the first pass found
landed in `[RECONSTRUCTED]` text, and every `[RETAINED]` section survived.**

Four blockers shaped the code:

**Booking a fee the vault never took.** `settle_close` takes `close_fee_usd` as
input and returns `close_fee_quote` as its *clamped* output — zero when equity is
non-positive. Booking the input made liabilities exceed the vault, so the
solvency invariant reverted the close and the position became permanently
unclosable, in a milestone that ships no keeper liquidation. The ledger books
`settled.close_fee_quote`. Three independent reviewers found this one.

**No exit that survives a dead feed.** All three close paths originally needed a
passing oracle read, so a feed that stopped publishing — a delisting, an outage,
or precisely the revocation the design exists for — trapped every position and
pinned LP capital behind the utilisation ceiling. `emergency_close_position` now
takes no price account, no pause gate, and settles from `last_good_price`, which
a *permissionless* `refresh_market_price` advances so an admin cannot freeze the
reference by pausing everything else.

**A liquidation fee that could underflow the transfer.** `apply_liquidation_fee`
clamps twice: against collateral, then against what the close fee left of the
gross.

**Eight instructions, one account constraint.** Anchor checks the discriminator,
program ownership and the seeds you write — nothing else. Every other binding was
an implementer's invention. `has_one = market`, the vault seeds, and the token
mint and owner pins are now explicit.

### LP share pricing ships as a bound, not a fix

Shares price off `pool.quote_deposited`, which ignores what open positions are
owed. The obvious fix — an aggregate mark — was designed, reviewed, and
**deleted for cause**: the pool's liability is the sum of
`max(0, min(equity, cap))` per position, and `max`/`min` do not commute with
summation. Two longs at +100 and −100 net to zero aggregate PnL while the pool
genuinely owes the winner 100, so a balanced two-sided book — the normal state of
a perp DEX — marks to a liability of zero.

No aggregate over `(Σ size, Σ entry_notional)` can compute it, and pricing off
`reserved_quote` instead is worse than the bug in both directions. So M5 ships
the bound: `M5_MAX_UTILIZATION_BPS = 2_000` caps reserved at 20% of AUM, which is
a provable ceiling on how far a share price can be overstated. `risk::pool::aum_usd`
stays uncalled and undeleted; §4.4 of the spec names what M6 must build to call it.

### Account layouts are size-locked

Every stage-3 field came out of `_reserved` rather than off the end — `Market`
128→96, `Position` 64→62, `Pool` 120→128 less the inert `min_liquidity_quote`.
Six compile-time `INIT_SPACE` asserts hold it there, because growing a struct
orphans every live devnet account rather than migrating it.

### Compute budget, measured

§9.11 asked for this "before the instruction set is frozen" and stage 3 shipped
without it. Measured in LiteSVM against the same program bytes now running on
devnet — `programs/` and `crates/risk` are unchanged between the deployed
commit `c058f40` and the measurement:

| Instruction | CU | of the 200,000 default |
|---|---:|---:|
| `open_position` | 38,905 | 19% |
| `close_position` | 34,344 | 17% |
| `emergency_close_position` | 32,694 | 16% |
| `refresh_market_price` | 9,330 | 4% |
| `settle_market` | 8,372 | 4% |

Everything fits the default per-instruction budget with roughly 5x headroom, so
no caller needs a `ComputeBudget` request. `close_position` was the one §9.11
named as the risk — an oracle read, funding and borrow settlement, fee maths and
a token-transfer CPI in one instruction — and it lands mid-table.

`the_position_lifecycle_fits_the_default_compute_budget` **asserts** the 200,000
bound rather than printing and moving on, so this cannot regress quietly once M6
and M7 add funding, borrow fees and liquidation to these same paths. The exact
digits are fixture-specific — one position, one market, one price path. The
margin is not.

### What milestone 5 does not settle

Deploying closes none of the spec's section 9. Ten items were open; §9.11 is now
answered, leaving nine. (§9.8 is a record of how refuter disagreements were
adjudicated, not an open item.)

| | Item | State |
|---|---|---|
| 9.11 | compute budget never measured | **closed** — above |
| 9.1 | M1 is bounded, not closed | open |
| 9.2 | the multi-hour funding farm | open |
| 9.3 | the reserve grief is priced, not eliminated | open |
| 9.4 | no keeper liquidation, load-bearing in two places | **closed** — `liquidate_position` |
| 9.5 | the staleness option is charged, but not proportionally to age | open |
| 9.6 | the admin is unrotatable | open |
| 9.7 | emergency close moves value at a price nobody chose | open |
| 9.9 | parts still tagged `[RECONSTRUCTED]`, therefore unreviewed | open |
| 9.10 | no test plan | open |

**9.6 is now the one to read before anyone puts real money near this.** The admin
key cannot be rotated. Devnet is the right place for this until that is addressed.

### Permissionless liquidation — closing 9.4

Deployed as `devnet-v0.7.0` (2026-08-26), `.so` sha256 `fcc3a2ba…e6973a1e`,
verified byte-identical on chain.

`liquidate_position` lets **anyone** close an underwater position and be paid for
it. It settles identically to `admin_settle_position` — same `LIQUIDATE` pause
gate, same liquidation oracle guards, same EMA clamp, same snapshotted spread,
same `is_liquidatable` gate at *current* notional, same two fee clamps in the
same order — and differs only in that the signer is unconstrained and takes
`exchange.keeper_fee_share_bps` of the fee.

**The keeper is paid out of the existing fee, never on top of it.** A liquidated
trader pays what `liquidation_fee_bps` always charged; only the destination of
part of it changes, so enabling keepers cannot reprice an open position. The
share is deducted *before* `book_fee`, because `book_fee`'s guarantee is that its
parts re-sum to what the vault **kept** — booking the whole fee and paying out
afterwards would credit liabilities the vault no longer backs and break I1.

`keeper_fee_share_bps` came out of `Exchange::_reserved` (96 → 94), so
`INIT_SPACE` stays 304 and the live exchange needed no reallocation — it read
**0** from its pre-existing reserve bytes, which is why liquidation became
permissionless the moment v0.7.0 landed while keepers earned nothing. It is now
set to **2000 (20%)** on devnet via `set_keeper_fee_share`. Capped at
`MAX_KEEPER_FEE_SHARE_BPS` (50%).

Eight tests cover it, including the two that pin the design rather than the
happy path: at a zero share `liquidate_position` settles byte-for-byte like
`admin_settle_position` across every payout field and the whole pool ledger —
the only configuration the deployed exchange is in — and at a non-zero share the
trader's payout is unchanged while the pool books exactly the keeper's share less.

### Proven on chain, 2026-08-30 — and it exposed two things

The first permissionless liquidation settled from `DT8fc3LE…3QfY`, a wallet with
no relationship to the position. Mechanically everything worked: the gate, the
account constraints, the ledger, the bad-debt record. Economically it did not.

**Accrual is crank-driven, not wall-clock.** `accrue` runs only when an
instruction touches the market. The position sat open for three days with
`cum_borrow_index` frozen at its opening 25,112 and `cum_funding_index` at 0 —
perfectly solvent, because nothing had settled it. One `settle_market` then
applied the whole backlog at once:

| | before | after |
|---|---:|---:|
| `cum_borrow_index` | 25,112 | 61,999,175 |
| `cum_funding_index` | 0 | 239,930,555 |

This is the load-bearing operational fact about the design and it is documented
nowhere else: **without something keeping the market settled, a position never
becomes liquidatable however long it sits.** Time alone does nothing.

**And the fee clamp is the default case, not an edge case.** Because the debt
landed in one step, the position went from solvent, past the liquidation band,
into bad debt without ever pausing in the window where a fee was payable. The
`PositionClosed` event:

    gross_payout_quote     0
    close_fee_quote        0
    liquidation_fee_quote  0
    net_payout_quote       0
    bad_debt_usd           1,690,280

The keeper paid gas and **earned nothing**. The trader received nothing. The pool
absorbed the collateral (`quote_deposited` 8,000,000 → 9,294,495) and booked
$1.69 of bad debt against `cum_bad_debt_usd`.

The two compound, and that is the finding: a missing crank does not merely delay
liquidation, it destroys the incentive meant to cause it. A keeper settling the
market on a schedule would have caught this position at the boundary and been
paid; a keeper that only watches arrives after the fee has clamped to zero.
**Any keeper for this venue must crank `settle_market`, not just observe.**

**Other known limits, pre-mainnet rather than devnet blockers.** A keeper can
stamp `market.last_good_price` from a price that passed only the looser
liquidation guards, and `emergency_close_position` settles off that field with no
freshness gate. Nothing relates `maintenance_margin_bps` to
`liquidation_max_confidence_bps + spread_bps`, leaving a band where an owner's own
`close_position` reverts while a keeper succeeds.

## Roadmap

- [x] **1** — repo, CI, first devnet deploy
- [x] **2** — risk core: fixed-point `i128`, zero floats, property tests
- [x] **3** — oracle adapter with staleness and confidence rejection
- [x] **4** — collateral vault
- [x] **5** — positions: open, close, isolated margin — *deployed `devnet-v0.6.0`*
- [ ] **6** — funding and borrow fees
- [ ] **7** — liquidation, insurance fund, bad-debt path
- [~] **8** — liquidation keeper — *on-chain half shipped (`liquidate_position`); no off-chain bot*
- [ ] **9** — TypeScript SDK and app integration
- [ ] **10** — hardening, fuzzing, reproducible builds, audit prep

Phases 2, 5 and 7 are the bulk of the work. Realistically this is months, not
weeks, and an audit adds months more on top.

## Security

See [SECURITY.md](SECURITY.md). No audit, no bug bounty yet. Reports are welcome
at **security@sakuraonseeker.com** and we will not pursue anyone testing in good
faith against devnet.

## History

This repository previously contained a fee router and a crank bot, preserved
under the tag `archive/fee-router-v0`. That code never compiled, could never have
worked (it used legacy SPL Token types against a Token-2022 mint), and was never
deployed to any cluster. Its crank bot decided whether to act by calling
`Math.random()`.

None of it is part of this project. It is kept for provenance, not reference.

## License

[Business Source License 1.1](LICENSE). Source-available: you may read, audit,
modify and run it on any test cluster. Running a production trading venue with it
requires a commercial licence until the change date of **2030-07-27**, when it
converts to Apache-2.0.
