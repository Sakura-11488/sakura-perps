# Sakura Perps

> ### ⚠️ Devnet only. Unaudited. Not for real funds.
>
> This program has not been audited, has never held anything of value, and is
> incomplete. Do not deploy it to mainnet. Do not send it money.

Permissionless oracle-and-pool perpetual futures on Solana.

**Status:** milestone 1 of 10 — pipeline established, engine not yet built.

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

> **Unresolved: `CollateralMintIsFreezable` blocks real USDC.**
> `initialize_exchange` rejects any collateral mint carrying a freeze authority
> (`lib.rs:90`). Verified on devnet against the deployed `devnet-v0.3.0`: it
> refuses Circle's devnet USDC, and mainnet USDC
> (`EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`) has freeze authority
> `7dGbd2QZ…` too. So the program as written can never take USDC on either
> cluster, which contradicts the collateral decision above. Needs resolving
> before the pool can be exercised end to end.

## Deployments

Source-to-bytecode traceability. Every deploy gets a row.

| Cluster | Program id | `.so` sha256 | Tag | Date | Upgrade authority |
|---|---|---|---|---|---|
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `cb1fee92…f717289c` | `devnet-v0.1.0` | 2026-07-27 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `569798fe…0e324d47` | `devnet-v0.2.0` | 2026-07-27 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `7c833439…310bbab3` | `devnet-v0.3.0` | 2026-07-28 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `5d879e58…07e61d2d` | `devnet-v0.4.0` | 2026-07-28 | `5JSAncTb…dKP` |
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | `6abbbf6d…fdba2a87` | `devnet-v0.5.0` | 2026-07-28 | `5JSAncTb…dKP` |

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

**Still missing: `cancel_withdraw`.** Request a withdrawal that then cannot
complete — utilisation rising above `max_utilization_bps` is enough — and the
shares sit in escrow with no way to take them back. `close_stale_escrow` does not
help, correctly: the escrow is non-empty and the request is live.

### Upgrading needs `program extend` first

The pool took the `.so` from 183,024 bytes to **418,896**, which does not fit the
ProgramData account the earlier deploys allocated — `solana program deploy` fails
on size before writing anything. Extend first, then deploy:

```bash
# 183,024 -> 524,288 (512 KiB), leaving headroom for the next milestone
solana program extend <program-id> 341264 --url "$RPC" -k <deployer>
solana program deploy target/deploy/sakura_perps.so \
  --program-id <program-id> --upgrade-authority <deployer> -k <deployer> --url "$RPC"
```

Two things that cost time here. Pass the program **address** to `--program-id`,
not a keypair path containing spaces — the CLI rejects that as an "unrecognized
signer source", and for an upgrade the address is what it wants anyway because
the upgrade authority is the signer. And `solana program dump` returns the whole
allocation, so the file is padded with zeros to 524,288; truncate to the `.so`
length before comparing hashes or it will never match:

```bash
solana program dump <program-id> onchain.so --url "$RPC"
head -c 418896 onchain.so | sha256sum   # == the artifact's sha256
```

`devnet-v0.3.0` was verified that way: on-chain bytes are byte-identical to the
CI artifact from run 30300839145 (`ac1cd48`), and the padding is all zero.

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

## Roadmap

- [x] **1** — repo, CI, first devnet deploy
- [ ] **2** — risk core: fixed-point `i128`, zero floats, property tests
- [ ] **3** — oracle adapter with staleness and confidence rejection
- [ ] **4** — collateral vault
- [ ] **5** — positions: open, close, isolated margin
- [ ] **6** — funding and borrow fees
- [ ] **7** — liquidation, insurance fund, bad-debt path
- [ ] **8** — liquidation keeper
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
