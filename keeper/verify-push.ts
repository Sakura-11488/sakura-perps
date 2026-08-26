/**
 * Milestone zero: prove a third party can refresh the pinned price feed.
 *
 *   npx ts-node keeper/verify-push.ts            # dry run, sends nothing
 *   PUSH=1 npx ts-node keeper/verify-push.ts     # actually pushes
 *
 * Env: RPC_URL, KEYPAIR.
 *
 * The entire keeper design rests on one claim: because `LiquidatePosition` pins
 * `address = market.price_update`, the bot cannot substitute its own price
 * account, so it must write into the market's own — and it is allowed to.
 *
 * That claim is INFERRED from the instruction being permissionless by
 * construction and from every observed write on devnet coming from an ordinary
 * third-party wallet. It has not been executed by us. If it is false, the atomic
 * design is dead and the keeper can only act during whatever minority of each
 * blackout the sponsored publisher happens to leave fresh.
 *
 * Success is NOT a confirmed signature. A confirmed transaction that did not move
 * `publish_time` means the update was not newer than what was already there —
 * someone else landed first, or Hermes served a stale VAA. That is a failed
 * experiment.
 */
import * as anchor from "@coral-xyz/anchor";
import { ComputeBudgetProgram, TransactionMessage, VersionedTransaction } from "@solana/web3.js";
import { loadConfig, loadKeypair, redactRpc } from "./src/config";
import { buildCtx, clusterNow } from "./src/program";
import { OracleUpdater, readPriceAge } from "./src/oracle";

/** SOL/USD, the feed this project's README names for devnet. */
const SOL_USD = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

async function main(): Promise<void> {
  const cfg = loadConfig();
  const keeper = loadKeypair(cfg.keypairPath);
  const ctx = await buildCtx(cfg, keeper);
  const updater = new OracleUpdater(ctx.connection, new anchor.Wallet(keeper));

  const account = updater.accountFor(SOL_USD);
  console.log(`rpc      ${redactRpc(cfg.rpcUrl)}`);
  console.log(`keeper   ${keeper.publicKey.toBase58()}`);
  console.log(`feed     ${account.toBase58()}`);
  console.log("         (this must equal the market's pinned price_update, or a push is useless)");

  const before = await readPriceAge(ctx.connection, account, await clusterNow(ctx.connection));
  if (!before) throw new Error("price account does not exist");
  console.log(
    `\nbefore   publish_time ${before.view.publishTime}  posted_slot ${before.view.postedSlot}` +
      `  age ${before.seconds}s / ${before.slots} slots`,
  );
  console.log(`         price ${before.view.price} x 10^${before.view.exponent}`);

  const ixs = await updater.buildPushInstructions(SOL_USD, before.view.publishTime);
  if (!ixs) {
    console.log("\nHermes has nothing newer than what is already on chain. Try again shortly.");
    console.log("This is not a failure — it is the guard that stops a non-newer push reverting.");
    return;
  }
  console.log(`\nbuilt    ${ixs.length} push instruction(s), no ephemeral signers`);

  if (process.env.PUSH !== "1") {
    console.log("\nDry run. Set PUSH=1 to actually send.");
    return;
  }

  const { blockhash, lastValidBlockHeight } = await ctx.connection.getLatestBlockhash("confirmed");
  const msg = new TransactionMessage({
    payerKey: keeper.publicKey,
    recentBlockhash: blockhash,
    instructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 400_000 }), ...ixs],
  }).compileToV0Message();
  const tx = new VersionedTransaction(msg);
  tx.sign([keeper]);

  const sig = await ctx.connection.sendTransaction(tx, { maxRetries: 3 });
  await ctx.connection.confirmTransaction(
    { signature: sig, blockhash, lastValidBlockHeight },
    "confirmed",
  );
  console.log(`sent     ${sig}`);

  const after = await readPriceAge(ctx.connection, account, await clusterNow(ctx.connection));
  if (!after) throw new Error("price account vanished");
  console.log(
    `after    publish_time ${after.view.publishTime}  posted_slot ${after.view.postedSlot}` +
      `  age ${after.seconds}s / ${after.slots} slots`,
  );

  const advanced =
    after.view.publishTime > before.view.publishTime &&
    after.view.postedSlot > before.view.postedSlot;

  console.log("");
  if (advanced) {
    console.log("RESULT: the push landed. publish_time and posted_slot both advanced.");
    console.log("The atomic oracle strategy is viable — a keeper can refresh the pinned feed.");
  } else {
    console.log("RESULT: INCONCLUSIVE. The transaction confirmed but the feed did not advance.");
    console.log("Someone else landed first, or the VAA was not newer. Re-run before concluding");
    console.log("anything; do NOT treat a confirmed signature alone as proof.");
    process.exitCode = 2;
  }
}

main().catch((e) => {
  console.error("verify-push failed:", e);
  process.exit(1);
});
