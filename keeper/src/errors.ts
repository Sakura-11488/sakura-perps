/**
 * Turning a failed simulation into a verdict.
 *
 * This is the most safety-critical file in the bot, and the reason is narrow:
 * the bot decides a position is HEALTHY by seeing a simulation fail. If any
 * other failure were also read as "healthy", one misderived account would make
 * every simulation fail, and the keeper would report an empty book, forever,
 * while being completely broken — and it would look exactly like a quiet day.
 *
 * So `healthy` is a closed set of one: `Custom(6062)` and nothing else.
 * Everything unrecognised is `unknown`, which alarms and eventually exits.
 */
import type { Idl } from "@coral-xyz/anchor";
import { POSITION_NOT_LIQUIDATABLE } from "./config";

export type Verdict =
  /** The simulation succeeded. This position can be liquidated right now. */
  | { kind: "liquidatable" }
  /** The program itself said no. The only benign refusal. */
  | { kind: "healthy" }
  /** The oracle is unusable. A MARKET-wide fact, never a statement about a position. */
  | { kind: "market-oracle"; code: number; name: string }
  /** Liquidation is paused. */
  | { kind: "paused" }
  /** Our accounts are wrong. A bug in the bot, not a property of the book. */
  | { kind: "misconfig"; code: number; name: string }
  /** The trader has no token account to be paid into. Skip, do not spend. */
  | { kind: "owner-account-missing" }
  /** Anything we cannot name. */
  | { kind: "unknown"; detail: string };

/**
 * Oracle failures. Every one of these is a property of the FEED, so a position
 * that returns one is `unknown`, never `healthy` — otherwise an oracle outage
 * silently records the entire book as solvent.
 */
const ORACLE_ERRORS = new Set([
  "OracleStale",
  "OraclePriceUnavailable",
  "OracleConfidenceTooWide",
  "OraclePriceOutOfBand",
  "OracleUnexpectedExponent",
  "OracleInvalidPrice",
  "OraclePriceFromTheFuture",
  "PriceDiverged",
  "WrongPriceUpdate",
]);

/** Account-shape failures: our derivation is wrong, and no retry will fix it. */
const MISCONFIG_ERRORS = new Set([
  "NotTokenOwner",
  "NotKeeperTokenOwner",
  "WrongCollateralMint",
  "WrongTokenProgram",
  "WrongMarket",
  "NotPositionOwner",
  "ConstraintSeeds",
]);

export interface CodeTable {
  byCode: Map<number, string>;
}

/**
 * Build the code→name table from the IDL that matches the deployed program, and
 * refuse to start if the one code the bot's logic depends on is not where we
 * think it is.
 *
 * This assertion exists because these discriminants have already shifted once in
 * this program's history: two error variants were inserted mid-enum and moved
 * four existing codes by two. Had the bot been running against the old table, it
 * would have read a real failure as `PositionNotLiquidatable` and skipped every
 * position in silence.
 */
export function buildCodeTable(idl: Idl): CodeTable {
  const byCode = new Map<number, string>();
  for (const e of idl.errors ?? []) byCode.set(e.code, e.name);

  const actual = byCode.get(POSITION_NOT_LIQUIDATABLE);
  if (actual !== "PositionNotLiquidatable") {
    throw new Error(
      `IDL mismatch: code ${POSITION_NOT_LIQUIDATABLE} is "${actual ?? "absent"}", ` +
        'expected "PositionNotLiquidatable". The IDL does not match the deployed ' +
        "program, and every healthy/unhealthy verdict this bot makes would be wrong.",
    );
  }
  return byCode.size > 0
    ? { byCode }
    : (() => {
        throw new Error("IDL carries no errors");
      })();
}

/** Pull the Anchor custom error code out of a simulation or send error. */
export function customCodeOf(err: unknown): number | null {
  if (err === null || err === undefined) return null;
  const asAny = err as Record<string, unknown>;
  const ie = asAny.InstructionError as unknown;
  if (Array.isArray(ie) && ie.length === 2) {
    const detail = ie[1] as Record<string, unknown>;
    if (detail && typeof detail === "object" && typeof detail.Custom === "number") {
      return detail.Custom;
    }
  }
  return null;
}

/** Which instruction index failed. A push failing is not a verdict on a position. */
export function failingIndexOf(err: unknown): number | null {
  const asAny = err as Record<string, unknown>;
  const ie = asAny?.InstructionError as unknown;
  if (Array.isArray(ie) && typeof ie[0] === "number") return ie[0];
  return null;
}

export function classify(err: unknown, logs: string[], table: CodeTable): Verdict {
  if (err === null || err === undefined) return { kind: "liquidatable" };

  const asAny = err as Record<string, unknown>;
  const ie = asAny.InstructionError as unknown;

  // A missing account is reported structurally, not as a custom code.
  if (Array.isArray(ie) && ie[1] === "AccountNotInitialized") {
    return { kind: "owner-account-missing" };
  }

  const code = customCodeOf(err);
  if (code === null) {
    return { kind: "unknown", detail: JSON.stringify(err).slice(0, 300) };
  }

  const name = table.byCode.get(code);
  if (name === undefined) {
    return { kind: "unknown", detail: `unmapped custom code ${code}` };
  }

  // The single benign refusal.
  if (code === POSITION_NOT_LIQUIDATABLE) {
    // Cross-check the log line. If the program named a different error while the
    // code said 6062, something is wrong enough that we must not skip quietly.
    const named = logs.find((l) => l.includes("Error Message:"));
    if (named && !named.includes("liquidatable") && !named.includes("Liquidatable")) {
      return { kind: "unknown", detail: `code 6062 but log says: ${named.slice(0, 160)}` };
    }
    return { kind: "healthy" };
  }

  if (ORACLE_ERRORS.has(name)) return { kind: "market-oracle", code, name };
  if (name === "LiquidationPaused") return { kind: "paused" };
  if (MISCONFIG_ERRORS.has(name)) return { kind: "misconfig", code, name };

  return { kind: "unknown", detail: `${name} (${code})` };
}

/** True when a verdict means "stop looking at this whole market this tick". */
export function isMarketWide(v: Verdict): boolean {
  return v.kind === "market-oracle" || v.kind === "paused";
}
