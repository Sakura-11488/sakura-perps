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
import { classify, CodeTable, failingIndexOf, Verdict } from "./errors";
import { measureKeeperFee, FeeMeasurement } from "./economics";

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
  const verdict = classify(res.value.err, logs, table);
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

export interface SendOutcome {
  sent: boolean;
  signature?: string;
  reason?: string;
}

/**
 * Rebuild at the real budget, re-verify those exact bytes, then send.
 *
 * `skipPreflight` is safe here — and only here — precisely because the bytes
 * about to be sent have just been simulated against the same blockhash.
 */
export async function sendVerified(
  ctx: Ctx,
  pushIxs: TransactionInstruction[],
  liquidateIx: TransactionInstruction,
  unitsConsumed: number,
  priorityFeeMicroLamports: number,
  keeper: Keypair,
): Promise<SendOutcome> {
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

  const recheck = await ctx.connection.simulateTransaction(tx, {
    sigVerify: false,
    replaceRecentBlockhash: false,
    commitment: "confirmed",
  });
  if (recheck.value.err !== null) {
    return {
      sent: false,
      reason: `recheck-failed:${JSON.stringify(recheck.value.err).slice(0, 120)}`,
    };
  }

  tx.sign([keeper]);
  const signature = await ctx.connection.sendTransaction(tx, {
    skipPreflight: true,
    maxRetries: 3,
  });
  await ctx.connection.confirmTransaction(
    { signature, blockhash, lastValidBlockHeight },
    "confirmed",
  );
  return { sent: true, signature };
}

/**
 * Ground truth for "did it actually liquidate".
 *
 * `close = owner` deallocates the position, so its absence is the only proof
 * that matters. A confirmed signature is not proof — losing the race to another
 * keeper also confirms, having done nothing.
 */
export async function positionGone(ctx: Ctx, position: PublicKey): Promise<boolean> {
  const info = await ctx.connection.getAccountInfo(position, "confirmed");
  return info === null;
}
