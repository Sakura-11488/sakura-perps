/**
 * Finding the book.
 *
 * Positions and markets are read with Anchor's typed account namespace, which
 * prepends the discriminator filter itself. No byte offsets appear anywhere in
 * this bot — the deployed IDL decodes these accounts correctly, so hand-rolled
 * layouts would be a second source of truth maintained by hand.
 */
import { PublicKey } from "@solana/web3.js";
import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import type { Ctx, ExchangeView } from "./program";

export interface PositionRow {
  address: PublicKey;
  owner: PublicKey;
  market: PublicKey;
  sizeBase: bigint;
  collateralQuote: bigint;
  entryPrice: bigint;
  maintenanceMarginBps: number;
}

export interface MarketRow {
  address: PublicKey;
  feedId: Buffer;
  priceUpdate: PublicKey;
  assetDecimals: number;
  liquidationMaxAgeSeconds: number;
  liquidationMaxAgeSlots: number;
  liquidationFeeBps: number;
  closeFeeBps: number;
  maintenanceMarginBps: number;
}

// deno-lint-ignore no-explicit-any
const big = (v: any): bigint => BigInt(v.toString());

export async function loadMarkets(ctx: Ctx): Promise<MarketRow[]> {
  const rows = await ctx.program.account.market.all();
  // deno-lint-ignore no-explicit-any
  return rows.map((r: any) => ({
    address: r.publicKey,
    feedId: Buffer.from(r.account.feedId),
    priceUpdate: r.account.priceUpdate,
    assetDecimals: r.account.assetDecimals,
    liquidationMaxAgeSeconds: r.account.liquidationMaxAgeSeconds,
    liquidationMaxAgeSlots: Number(r.account.liquidationMaxAgeSlots.toString()),
    liquidationFeeBps: r.account.liquidationFeeBps,
    closeFeeBps: r.account.closeFeeBps,
    maintenanceMarginBps: r.account.maintenanceMarginBps,
  }));
}

export async function loadPositions(ctx: Ctx): Promise<PositionRow[]> {
  const rows = await ctx.program.account.position.all();
  // deno-lint-ignore no-explicit-any
  return rows.map((r: any) => ({
    address: r.publicKey,
    owner: r.account.owner,
    market: r.account.market,
    sizeBase: big(r.account.sizeBase),
    collateralQuote: big(r.account.collateralQuote),
    entryPrice: big(r.account.entryPrice),
    maintenanceMarginBps: r.account.maintenanceMarginBps,
  }));
}

export function groupByMarket(positions: PositionRow[]): Map<string, PositionRow[]> {
  const out = new Map<string, PositionRow[]>();
  for (const p of positions) {
    const k = p.market.toBase58();
    const list = out.get(k);
    if (list) list.push(p);
    else out.set(k, [p]);
  }
  return out;
}

/**
 * Where the trader's settlement is paid.
 *
 * The program constrains this account only on mint and owner, not on it being
 * the canonical ATA. So when the ATA does not exist, any other token account the
 * owner holds for this mint is equally valid — and using it strictly enlarges
 * the set of positions the keeper can liquidate rather than skipping them.
 *
 * Returns null when the trader has no account at all. That position is skipped,
 * not funded: creating an account on the trader's behalf costs ~0.002 SOL of
 * unrecoverable rent, which is roughly four hundred times every other cost the
 * keeper pays and would be spent for someone else's benefit.
 */
export async function resolveOwnerTokenAccount(
  ctx: Ctx,
  exchange: ExchangeView,
  owner: PublicKey,
): Promise<PublicKey | null> {
  const ata = getAssociatedTokenAddressSync(
    exchange.collateralMint,
    owner,
    true,
    exchange.collateralTokenProgram,
  );
  const info = await ctx.connection.getAccountInfo(ata, "confirmed");
  if (info) return ata;

  const owned = await ctx.connection.getTokenAccountsByOwner(
    owner,
    { mint: exchange.collateralMint },
    { commitment: "confirmed" },
  );
  return owned.value.length > 0 ? owned.value[0].pubkey : null;
}
