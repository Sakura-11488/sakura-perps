/**
 * Every tunable the keeper has, named once, here.
 *
 * Thresholds scattered through a bot are how it ends up with two different ideas
 * of "too expensive". If a number decides something, it lives in this file.
 */
import { PublicKey } from "@solana/web3.js";
import { Keypair } from "@solana/web3.js";
import fs from "fs";

export const PROGRAM_ID = new PublicKey("5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y");

/**
 * Devnet's genesis hash.
 *
 * The bot refuses to run against a cluster that is not this one. A mistyped
 * RPC_URL is otherwise indistinguishable from a correct one right up until an
 * unaudited keeper starts signing against mainnet.
 */
export const DEVNET_GENESIS = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

/** Anchor error code for `PositionNotLiquidatable`, asserted against the IDL at boot. */
export const POSITION_NOT_LIQUIDATABLE = 6062;

const num = (name: string, dflt: number): number => {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return dflt;
  const v = Number(raw);
  if (!Number.isFinite(v)) throw new Error(`${name} must be a number, got ${raw}`);
  return v;
};

/** Env booleans are opt-OUT for safety flags and opt-IN for spending ones. */
const bool = (name: string, dflt: boolean): boolean => {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return dflt;
  return raw === "1" || raw.toLowerCase() === "true";
};

export interface Config {
  rpcUrl: string;
  keypairPath: string;
  dryRun: boolean;
  tickMs: number;
  maxSimsPerTick: number;
  minKeeperFeeQuote: number;
  priorityFeeShareBps: number;
  priorityFeeCapLamports: number;
  maxSendsPerTick: number;
  maxSendsPerHour: number;
  maxDailyLamports: number;
  minSolLamports: number;
  exitOnUnknown: boolean;
  unknownTolerance: number;
  separatePush: boolean;
  allowAnyCluster: boolean;
  marketRefreshTicks: number;
  heartbeatUrl?: string;
}

export function loadConfig(): Config {
  const rpcUrl = process.env.RPC_URL;
  const keypairPath = process.env.KEYPAIR;
  if (!rpcUrl) throw new Error("set RPC_URL");
  if (!keypairPath) throw new Error("set KEYPAIR (a filesystem path)");

  // A path, never inline key material. An env var holding a secret key is
  // visible to `ps` and lands in shell history and crash dumps.
  if (keypairPath.trim().startsWith("[") || keypairPath.length > 512) {
    throw new Error("KEYPAIR must be a path to a keypair file, not the key itself");
  }

  return {
    rpcUrl,
    keypairPath,
    // Defaults ON. A first run must not be able to spend money while the
    // account derivations are still unproven.
    dryRun: bool("DRY_RUN", true),
    tickMs: num("TICK_MS", 2000),
    maxSimsPerTick: num("MAX_SIMS_PER_TICK", 40),
    /** $0.01 at 6 decimals. Below this a liquidation cannot pay for its own signature. */
    minKeeperFeeQuote: num("MIN_KEEPER_FEE_QUOTE", 10_000),
    priorityFeeShareBps: num("PRIORITY_FEE_SHARE_BPS", 2500),
    priorityFeeCapLamports: num("PRIORITY_FEE_CAP_LAMPORTS", 200_000),
    maxSendsPerTick: num("MAX_SENDS_PER_TICK", 3),
    maxSendsPerHour: num("MAX_SENDS_PER_HOUR", 60),
    maxDailyLamports: num("MAX_DAILY_LAMPORTS", 50_000_000),
    minSolLamports: num("MIN_SOL_LAMPORTS", 50_000_000),
    exitOnUnknown: bool("EXIT_ON_UNKNOWN", true),
    unknownTolerance: num("UNKNOWN_TOLERANCE", 3),
    separatePush: bool("SEPARATE_PUSH", false),
    allowAnyCluster: bool("ALLOW_ANY_CLUSTER", false),
    marketRefreshTicks: num("MARKET_REFRESH_TICKS", 30),
    heartbeatUrl: process.env.HEARTBEAT_URL,
  };
}

export function loadKeypair(path: string): Keypair {
  const home = process.env.HOME ?? process.env.USERPROFILE ?? "";
  const raw = JSON.parse(fs.readFileSync(path.replace("~", home), "utf8"));
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

/**
 * Scheme and host only.
 *
 * Every RPC URL in this project carries an API key in its query string. Logs
 * get pasted into issues; metrics get shipped to third parties. Nothing here
 * emits a URL that has not been through this function.
 */
export function redactRpc(url: string): string {
  try {
    const u = new URL(url);
    return `${u.protocol}//${u.host}`;
  } catch {
    return "<unparseable-url>";
  }
}
