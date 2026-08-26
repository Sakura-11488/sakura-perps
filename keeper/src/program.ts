/**
 * Connection, program handle and the PDAs everything else derives from.
 *
 * Imports the tracked `idl/sakura_perps.ts` rather than `target/types/...`,
 * because `target/` is gitignored — a fresh clone has the IDL but not the
 * generated types, and a keeper that only builds on the machine that last ran
 * `anchor build` is not deployable.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import fs from "fs";
import path from "path";
import type { SakuraPerps } from "../../idl/sakura_perps";
import { Config, DEVNET_GENESIS, PROGRAM_ID, redactRpc } from "./config";

export interface Ctx {
  connection: Connection;
  program: Program<SakuraPerps>;
  idl: anchor.Idl;
  keeper: Keypair;
  exchange: PublicKey;
  pool: PublicKey;
  quoteVault: PublicKey;
}

const repoRoot = path.resolve(__dirname, "..", "..");

export function loadIdl(): anchor.Idl {
  return JSON.parse(fs.readFileSync(path.join(repoRoot, "idl", "sakura_perps.json"), "utf8"));
}

export async function buildCtx(cfg: Config, keeper: Keypair): Promise<Ctx> {
  const connection = new Connection(cfg.rpcUrl, "confirmed");

  // Refuse an unexpected cluster before anything is signed. A wrong RPC_URL is
  // otherwise silent right up until the first mainnet signature.
  if (!cfg.allowAnyCluster) {
    const genesis = await connection.getGenesisHash();
    if (genesis !== DEVNET_GENESIS) {
      throw new Error(
        `refusing to run: ${redactRpc(cfg.rpcUrl)} has genesis ${genesis}, ` +
          `expected devnet ${DEVNET_GENESIS}. Set ALLOW_ANY_CLUSTER=1 only if you mean it.`,
      );
    }
  }

  const provider = new anchor.AnchorProvider(connection, new anchor.Wallet(keeper), {
    commitment: "confirmed",
  });
  const idl = loadIdl();
  const program = new Program(idl as anchor.Idl, provider) as unknown as Program<SakuraPerps>;

  const [exchange] = PublicKey.findProgramAddressSync([Buffer.from("exchange")], PROGRAM_ID);
  const [pool] = PublicKey.findProgramAddressSync([Buffer.from("pool")], PROGRAM_ID);
  const [quoteVault] = PublicKey.findProgramAddressSync([Buffer.from("quote_vault")], PROGRAM_ID);

  return { connection, program, idl, keeper, exchange, pool, quoteVault };
}

export interface ExchangeView {
  collateralMint: PublicKey;
  collateralTokenProgram: PublicKey;
  collateralDecimals: number;
  pausedFlags: bigint;
  keeperFeeShareBps: number;
  numMarkets: number;
}

/**
 * Re-read every tick-cadence because all of these are admin-mutable underneath a
 * running bot: the pause bitfield, the keeper's share, and the market count.
 * Caching them for the process lifetime means the bot keeps acting on a
 * configuration that changed hours ago.
 */
export async function readExchange(ctx: Ctx): Promise<ExchangeView> {
  const e = await ctx.program.account.exchange.fetch(ctx.exchange);
  return {
    collateralMint: e.collateralMint as PublicKey,
    collateralTokenProgram: e.collateralTokenProgram as PublicKey,
    collateralDecimals: e.collateralDecimals as number,
    pausedFlags: BigInt((e.pausedFlags as anchor.BN).toString()),
    keeperFeeShareBps: e.keeperFeeShareBps as number,
    numMarkets: e.numMarkets as number,
  };
}

/** Cluster time, not wall time. A skewed VPS clock mis-sizes every age decision. */
export async function clusterNow(connection: Connection): Promise<{ unix: number; slot: number }> {
  const slot = await connection.getSlot("confirmed");
  const unix = await connection.getBlockTime(slot);
  if (unix === null) throw new Error(`no block time for slot ${slot}`);
  return { unix, slot };
}
