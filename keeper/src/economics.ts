/**
 * Whether a liquidation is worth sending.
 *
 * The keeper's income is a share of the liquidation fee, and that fee is clamped
 * twice on chain: against the position's remaining collateral, and against what
 * the close fee left of the payout. The second clamp is the one that bites here.
 * Once a position is far enough underwater the fee — and so the keeper's cut —
 * is ZERO, which means the bot earns nothing on precisely the positions whose
 * bad debt matters most to the pool.
 *
 * That is a real limitation of the incentive, not of this code, and it is why
 * SUBSIDISED exists: an operator who cares about the protocol's solvency rather
 * than the fee can choose to liquidate at a loss. It is off by default, because
 * a bot that silently pays to work is a bot nobody notices burning a wallet.
 */
import { PublicKey } from "@solana/web3.js";
import type { Config } from "./config";

/** The measured outcome of a simulation, in collateral base units. */
export interface FeeMeasurement {
  keeperFeeQuote: bigint;
  source: "balance-diff" | "unavailable";
}

/**
 * Read the keeper's fee from the simulation itself rather than recomputing it.
 *
 * `simulateTransaction` can return post-execution account data. Diffing the SPL
 * token amount (u64 LE at offset 64) of the keeper's own account against its
 * pre-balance gives the exact figure the program would pay, with every clamp and
 * rounding rule already applied. Recomputing it here would be a second
 * implementation of money math, and it would be wrong at exactly the boundary
 * where all liquidations live.
 */
export function measureKeeperFee(
  postAccountData: Buffer | null,
  preBalance: bigint,
): FeeMeasurement {
  if (!postAccountData || postAccountData.length < 72) {
    return { keeperFeeQuote: 0n, source: "unavailable" };
  }
  const post = postAccountData.readBigUInt64LE(64);
  const delta = post > preBalance ? post - preBalance : 0n;
  return { keeperFeeQuote: delta, source: "balance-diff" };
}

export type Decision =
  { act: true; priorityFeeMicroLamports: number } | { act: false; reason: string };

/**
 * The profitability gate.
 *
 * `subsidised` deliberately bypasses only the fee floor, never the caps — an
 * operator willing to work at a loss still should not be able to burn the wallet
 * in a loop.
 */
export function decide(
  fee: FeeMeasurement,
  cfg: Config,
  subsidised: boolean,
  unitsConsumed: number,
): Decision {
  if (fee.source === "unavailable") {
    // Refuse rather than guess. An unmeasurable fee is the case where a bug in
    // the accounts would show up as "free money".
    return { act: false, reason: "fee-unmeasurable" };
  }
  if (!subsidised && fee.keeperFeeQuote < BigInt(cfg.minKeeperFeeQuote)) {
    return {
      act: false,
      reason: `fee-below-floor(${fee.keeperFeeQuote} < ${cfg.minKeeperFeeQuote})`,
    };
  }

  // Bid a bounded fraction of what we are about to earn, never more than the
  // hard cap. Winning a race at a loss is still a loss.
  const units = Math.max(1, unitsConsumed);
  const budgetLamports = Math.min(
    cfg.priorityFeeCapLamports,
    Math.floor((Number(fee.keeperFeeQuote) * cfg.priorityFeeShareBps) / 10_000),
  );
  const micro = Math.max(0, Math.floor((budgetLamports * 1_000_000) / units));
  return { act: true, priorityFeeMicroLamports: micro };
}

/**
 * How wide the window is between "liquidatable" and "the fee has clamped to
 * nothing", in basis points of notional.
 *
 * If this is zero or negative the market can never pay a keeper, and the
 * operator should know at boot rather than after a week of silence.
 */
export function profitWindowBps(m: {
  maintenanceMarginBps: number;
  closeFeeBps: number;
  liquidationFeeBps: number;
}): number {
  return m.maintenanceMarginBps - m.closeFeeBps - m.liquidationFeeBps;
}

export const KEEPER_ATA_TOKEN_AMOUNT_OFFSET = 64;

export function ataOf(
  mint: PublicKey,
  owner: PublicKey,
  tokenProgram: PublicKey,
  getAta: (m: PublicKey, o: PublicKey, off: boolean, tp: PublicKey) => PublicKey,
): PublicKey {
  return getAta(mint, owner, true, tokenProgram);
}
