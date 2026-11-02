import type { Connection } from "@solana/web3.js";
import { Journal, lamports, PendingTransaction } from "./journal";

export interface Resolution {
  pending: PendingTransaction;
  kind: "finalized" | "expired";
  failed: boolean;
  cost: number;
}

/** Unknown RPC outcomes never unlock. Finalized metadata is the spending receipt. */
export async function reconcilePending(
  connection: Pick<Connection, "getSignatureStatuses" | "getTransaction" | "getBlockHeight">,
  journal: Journal,
  now: number,
): Promise<Resolution[]> {
  const resolutions: Resolution[] = [];
  for (const pending of journal.pending) {
    const statuses = await connection.getSignatureStatuses([pending.signature], {
      searchTransactionHistory: true,
    });
    const status = statuses.value[0];
    if (status?.confirmationStatus === "finalized") {
      const tx = await connection.getTransaction(pending.signature, {
        commitment: "finalized",
        maxSupportedTransactionVersion: 0,
      });
      // Missing metadata is not evidence of zero spend. Keep its reservation.
      if (
        !tx?.meta ||
        tx.meta.preBalances[0] === undefined ||
        tx.meta.postBalances[0] === undefined
      )
        continue;
      const cost = lamports(
        Math.max(tx.meta.fee, tx.meta.preBalances[0] - tx.meta.postBalances[0], 0),
      );
      journal.settle(pending.signature, cost, now);
      resolutions.push({ pending, kind: "finalized", failed: tx.meta.err !== null, cost });
    } else if (status === null) {
      const height = await connection.getBlockHeight("finalized");
      if (height <= pending.lastValidBlockHeight) continue;
      // Re-read after the finalized height: the first status may predate finalization.
      const after = await connection.getSignatureStatuses([pending.signature], {
        searchTransactionHistory: true,
      });
      if (after.value[0] !== null) continue;
      // A finalized RPC receipt can exist even when its signature index lags.
      const tx = await connection.getTransaction(pending.signature, {
        commitment: "finalized",
        maxSupportedTransactionVersion: 0,
      });
      if (tx !== null) continue;
      // An archival gap cannot prove it never cost anything. Charge the full
      // reservation rather than refunding an unprovable zero-cost submission.
      journal.settle(pending.signature, pending.reservedLamports, now);
      resolutions.push({ pending, kind: "expired", failed: true, cost: pending.reservedLamports });
    }
  }
  return resolutions;
}

/**
 * The measured payer debit includes oracle publication/rent. Add the network
 * fee separately (even if the simulator included it), plus a cushion; reconcile
 * against finalized balances and stop if execution exceeds this reservation.
 */
export function reserveCost(preBalance: number, postBalance: number, networkFee: number): number {
  lamports(preBalance);
  lamports(postBalance);
  lamports(networkFee);
  const estimate = lamports(Math.max(0, preBalance - postBalance) + networkFee);
  return lamports(estimate + Math.max(5_000, Math.ceil(estimate / 5)));
}

/** Persist reservation and signature before the first byte can reach an RPC. */
export async function broadcastReserved(
  journal: Journal,
  pending: Omit<PendingTransaction, "day">,
  dailyLimit: number,
  hourlyLimit: number,
  broadcast: () => Promise<string>,
): Promise<void> {
  journal.reserve(pending, dailyLimit, hourlyLimit);
  const returned = await broadcast();
  if (returned !== pending.signature)
    throw new Error(
      "RPC returned a different signature; pending transaction requires reconciliation",
    );
}
