# Sakura Perps — devnet dashboard

A **read-only**, live dashboard for the [sakura-perps](https://github.com/Sakura-11488/sakura-perps)
program on Solana **devnet**. It reads on-chain state and renders it — it sends
nothing on-chain, signs nothing, and needs no funds.

Built entirely against the repo's **committed IDL** (`../idl/sakura_perps.json`),
so it runs with just Node — no Anchor / Solana / Rust toolchain and no
`anchor build` required.

> A starting point for the phase-9 "TypeScript SDK and app integration" work.

## What it shows

- **Live SOL/USD** from the Pyth `PriceUpdateV2` account the program prices
  against — decoded directly, with a rolling sparkline, tick flash, and session
  high/low.
- **Oracle guard status** — a live re-implementation of the program's
  `for_trading` guards (`crates/risk/src/oracle.rs`): exponent, future-skew,
  upstream freshness, on-chain slot age, confidence width. Each row shows the
  exact error the program would throw. A toggle switches to the looser
  `for_liquidation` thresholds.
- **Exchange** config, **Pool** state with a utilization gauge, open **withdraw
  requests** (with escrow status), and a **recent activity** feed.
- Explorer links on every address, auto-refresh, light / dark theme.

## Run it

```bash
cd dashboard
npm install
cp .env.example .env.local   # then set RPC to any Solana devnet endpoint
npm run dev                  # http://localhost:3939
```

It reuses the repo's `idl/sakura_perps.json`, so it always reflects the IDL
checked in alongside the program.

Any devnet RPC works; a dedicated one (e.g. Helius) avoids public-endpoint
throttling. The RPC is read server-side only and never shipped to the browser.

## How it reads the program

`lib/chain.js` points `@coral-xyz/anchor` at the deployed devnet program using
the IDL and a throwaway (never-signing) wallet, then fetches accounts by their
PDA seeds — the same seeds the program uses. A lightweight `/api/price` route
polls just the oracle every few seconds; the full state refreshes less often.

## License

Follows the upstream program: read, audit, run on any test cluster.
