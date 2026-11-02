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
- **Oracle reference checks** — compares the observed price with the default
  `for_trading` / `for_liquidation` thresholds in `crates/risk/src/oracle.rs`.
  These are **not the live market's configuration or a program-acceptance verdict**.
  Market guards, sanity bands, feed revocation, pause state, EMA divergence and
  margin are not evaluated. Missing checks remain unknown, even when every
  reference check passes. Freshness uses the on-chain Clock sysvar; clock failures
  leave the time and slot checks unknown.
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
API errors use fixed codes and do not include provider exception messages. All
responses are `no-store`; `/api/price` returns HTTP 503 when the account cannot be
read or validated. Partial `/api/state` reads return `status: "degraded"` with
fixed section error codes. If a credential was exposed by an earlier dashboard
version, rotate it; removing the response field does not revoke an exposed key.

## How it reads the program

`lib/chain.js` points `@coral-xyz/anchor` at the deployed devnet program using
the IDL and a throwaway (never-signing) wallet, then fetches accounts by their
PDA seeds — the same seeds the program uses. A lightweight `/api/price` route
polls the oracle and cluster clock every few seconds; the full state refreshes
less often. Oracle decoding verifies the receiver owner, Anchor discriminator,
full verification level, SOL/USD feed identity, layout and numeric bounds before
displaying a price. Failed polling removes the previous oracle verdict; snapshots
older than 15 seconds are treated as unavailable.

## Security checks

```bash
npm test
npm run build
```

The offline regression suite checks API credential non-disclosure with sentinel
secrets, bounded error responses, unknown guard state, clock failures, future
prices and malformed oracle accounts. It sends no live transactions.

## License

Follows the upstream program: read, audit, run on any test cluster.
