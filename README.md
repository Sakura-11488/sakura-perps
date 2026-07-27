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
| Collateral (planned) | USDC-devnet `Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr` |
| Oracle receiver | Pyth `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` |
| SOL/USD feed | `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` |

Note that USDC-devnet is owned by the **legacy** SPL Token program while SAKURA
is Token-2022. The program uses `anchor_spl::token_interface` throughout so it
can accept either, and pins whichever it was initialized with.

## Deployments

Source-to-bytecode traceability. Every deploy gets a row.

| Cluster | Program id | `.so` sha256 | Tag | Date | Upgrade authority |
|---|---|---|---|---|---|
| devnet | `5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y` | _pending first deploy_ | — | — | deployer key |

Upgrade authority is a single key while on devnet. Before any mainnet
consideration it moves to a Squads multisig behind a timelock, and there will
never be an instruction permitting the admin to move funds out of the vault.

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
