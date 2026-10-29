/**
 * Building, simulating and sending one liquidation.
 *
 * Two phases, and the second is not optional. Phase one simulates with a
 * placeholder compute budget to learn the verdict, the units consumed and the
 * fee. Phase two rebuilds the transaction with the REAL compute budget and the
 * REAL blockhash, and re-simulates THOSE EXACT BYTES before signing.
 *
 * Skipping phase two is the classic way to send something you never verified:
 * rewriting the compute-budget instructions changes the message, and the message
 * you measured is then not the message you signed.
 */
import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import type { Ctx, ExchangeView } from "./program";
import type { MarketRow, PositionRow } from "./discover";
import { classifyLiquidation, CodeTable, failingIndexOf, Verdict } from "./errors";
import { measureKeeperFee, FeeMeasurement } from "./economics";
import { utils } from "@coral-xyz/anchor";
import { reserveCost } from "./pending";
import { PROGRAM_ID } from "./config";

export interface Candidate {
  position: PositionRow;
  market: MarketRow;
  ownerTokenAccount: PublicKey;
}

export async function buildLiquidateIx(
  ctx: Ctx,
  exchange: ExchangeView,
  c: Candidate,
): Promise<TransactionInstruction> {
  const keeperTokenAccount = getAssociatedTokenAddressSync(
    exchange.collateralMint,
    ctx.keeper.publicKey,
    true,
    exchange.collateralTokenProgram,
  );

  return ctx.program.methods
    .liquidatePosition()
    .accountsPartial({
      exchange: ctx.exchange,
      keeper: ctx.keeper.publicKey,
      pool: ctx.pool,
      market: c.market.address,
      priceUpdate: c.market.priceUpdate,
      owner: c.position.owner,
      position: c.position.address,
      collateralMint: exchange.collateralMint,
      quoteVault: ctx.quoteVault,
      ownerTokenAccount: c.ownerTokenAccount,
      keeperTokenAccount,
      tokenProgram: exchange.collateralTokenProgram,
    })
    .instruction();
}

export interface SimResult {
  verdict: Verdict;
  unitsConsumed: number;
  fee: FeeMeasurement;
  logs: string[];
  /** Set when the failure was in a push instruction rather than the liquidation. */
  failedInstructionIndex: number | null;
}

async function tokenAmount(connection: Connection, account: PublicKey): Promise<bigint> {
  const info = await connection.getAccountInfo(account, "confirmed");
  if (!info || info.data.length < 72) return 0n;
  return info.data.readBigUInt64LE(64);
}

export async function simulate(
  ctx: Ctx,
  exchange: ExchangeView,
  pushIxs: TransactionInstruction[],
  liquidateIx: TransactionInstruction,
  table: CodeTable,
): Promise<SimResult> {
  if (!liquidateIx.programId.equals(PROGRAM_ID))
    throw new Error("liquidation instruction targets an unexpected program");
  const keeperAta = getAssociatedTokenAddressSync(
    exchange.collateralMint,
    ctx.keeper.publicKey,
    true,
    exchange.collateralTokenProgram,
  );
  const pre = await tokenAmount(ctx.connection, keeperAta);

  const ixs = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    ...pushIxs,
    liquidateIx,
  ];
  const msg = new TransactionMessage({
    payerKey: ctx.keeper.publicKey,
    // Replaced by the simulator; a real blockhash is not needed to learn a verdict.
    recentBlockhash: PublicKey.default.toBase58(),
    instructions: ixs,
  }).compileToV0Message();
  const tx = new VersionedTransaction(msg);

  const res = await ctx.connection.simulateTransaction(tx, {
    sigVerify: false,
    replaceRecentBlockhash: true,
    commitment: "confirmed",
    accounts: { encoding: "base64", addresses: [keeperAta.toBase58()] },
  });

  const logs = res.value.logs ?? [];
  const verdict = classifyLiquidation(
    res.value.err,
    logs,
    table,
    pushIxs.length + 1,
    PROGRAM_ID.toBase58(),
  );
  const postRaw = res.value.accounts?.[0]?.data?.[0];
  const post = postRaw ? Buffer.from(postRaw, "base64") : null;

  return {
    verdict,
    unitsConsumed: res.value.unitsConsumed ?? 0,
    fee:
      verdict.kind === "liquidatable"
        ? measureKeeperFee(post, pre)
        : { keeperFeeQuote: 0n, source: "unavailable" },
    logs,
    failedInstructionIndex: failingIndexOf(res.value.err),
  };
}

export interface PreparedTransaction {
  bytes: Uint8Array;
  signature: string;
  blockhash: string;
  lastValidBlockHeight: number;
  reservedLamports: number;
}

/**
 * Rebuild and verify the exact message; signing alone never broadcasts.
 *
 * `skipPreflight` is safe here — and only here — precisely because the bytes
 * about to be sent have just been simulated against the same blockhash.
 */
export async function prepareVerified(
  ctx: Ctx,
  pushIxs: TransactionInstruction[],
  liquidateIx: TransactionInstruction,
  unitsConsumed: number,
  priorityFeeMicroLamports: number,
  keeper: Keypair,
): Promise<{ prepared: PreparedTransaction } | { reason: string }> {
  const units = Math.min(1_400_000, Math.ceil(Math.max(unitsConsumed, 50_000) * 1.2));
  const ixs = [
    ComputeBudgetProgram.setComputeUnitLimit({ units }),
    ComputeBudgetProgram.setComputeUnitPrice({ microLamports: priorityFeeMicroLamports }),
    ...pushIxs,
    liquidateIx,
  ];

  const { blockhash, lastValidBlockHeight } = await ctx.connection.getLatestBlockhash("confirmed");
  const msg = new TransactionMessage({
    payerKey: keeper.publicKey,
    recentBlockhash: blockhash,
    instructions: ixs,
  }).compileToV0Message();
  const tx = new VersionedTransaction(msg);

  const fee = await ctx.connection.getFeeForMessage(msg, "confirmed");
  if (fee.value === null) return { reason: "network-fee-unavailable" };
  let balance = await ctx.connection.getBalanceAndContext(keeper.publicKey, "confirmed");
  const recheck = await ctx.connection.simulateTransaction(tx, {
    sigVerify: false,
    replaceRecentBlockhash: false,
    commitment: "confirmed",
    minContextSlot: balance.context.slot,
    accounts: { encoding: "base64", addresses: [keeper.publicKey.toBase58()] },
  });
  if (recheck.value.err !== null) {
    return {
      reason: `recheck-failed:${JSON.stringify(recheck.value.err).slice(0, 120)}`,
    };
  }

  const postBalance = recheck.value.accounts?.[0]?.lamports;
  if (postBalance === undefined) return { reason: "payer-cost-unavailable" };
  if (balance.context.slot !== recheck.context.slot) {
    balance = await ctx.connection.getBalanceAndContext(keeper.publicKey, {
      commitment: "confirmed",
      minContextSlot: recheck.context.slot,
    });
  }
  // A credit between unmatched snapshots could hide publication/rent costs.
  // Require the same frozen confirmed bank, rather than subtracting arbitrary reads.
  if (balance.context.slot !== recheck.context.slot) return { reason: "payer-snapshot-changed" };
  const reservedLamports = reserveCost(balance.value, postBalance, fee.value);
  tx.sign([keeper]);
  return {
    prepared: {
      bytes: tx.serialize(),
      signature: utils.bytes.bs58.encode(tx.signatures[0]),
      blockhash,
      lastValidBlockHeight,
      reservedLamports,
    },
  };
}

/**
 * Ground truth for "did it actually liquidate".
 *
 * `close = owner` deallocates the position, so its absence is the only proof
 * that matters. A confirmed signature is not proof — losing the race to another
 * keeper also confirms, having done nothing.
 */
export async function positionGone(ctx: Ctx, position: PublicKey): Promise<boolean> {
  const info = await ctx.connection.getAccountInfo(position, "finalized");
  return info === null;
}
