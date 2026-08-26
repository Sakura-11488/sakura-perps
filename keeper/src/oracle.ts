/**
 * Keeping the pinned price account fresh enough to liquidate against.
 *
 * `LiquidatePosition` declares
 *   #[account(address = market.price_update @ WrongPriceUpdate)]
 * so the keeper CANNOT post its own ephemeral `PriceUpdateV2` and pass that. The
 * only route is writing into that exact account, which for a Pyth push-oracle
 * feed is the shard-0 PDA under `pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT`.
 * Verified: `getPriceFeedAccountAddress(0, SOL/USD)` derives
 * `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`, which is the feed this
 * project's README names, and it is writable by anyone.
 *
 * Why this file exists at all: devnet's sponsored publisher is intermittent. It
 * was measured going 314 seconds between writes, and slot age reaching ~1500 —
 * far outside any sane liquidation guard. A keeper that waits for someone else
 * to push would be able to act during a small minority of each blackout, in
 * windows it does not choose. So the bot carries its own update.
 *
 * The push instruction is always in the SAME transaction as the liquidation.
 * That is deliberate twice over: it removes the race between refreshing and
 * acting, and it means the keeper never pays to publish a price unless it is
 * being paid to liquidate. A keeper that pushes on a schedule is just an unpaid
 * Pyth publisher.
 */
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { HermesClient } from "@pythnetwork/hermes-client";
import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey, TransactionInstruction } from "@solana/web3.js";

const HERMES_URL = "https://hermes.pyth.network";

/** The `PriceUpdateV2` layout, as served by the Pyth receiver program. */
export interface PriceView {
  feedId: Buffer;
  price: bigint;
  confidence: bigint;
  exponent: number;
  publishTime: number;
  postedSlot: number;
  verificationLevel: "full" | "partial";
}

/**
 * Decode a `PriceUpdateV2`.
 *
 * 8 discriminator, 32 write_authority, 1 verification level (+1 byte payload
 * when Partial), 32 feed id, then the price message.
 */
export function decodePriceUpdate(data: Buffer): PriceView {
  let o = 8 + 32;
  const level = data.readUInt8(o);
  // Partial carries a u8 threshold; Full does not.
  o += level === 0 ? 2 : 1;
  const feedId = data.subarray(o, o + 32);
  o += 32;
  const price = data.readBigInt64LE(o);
  o += 8;
  const confidence = data.readBigUInt64LE(o);
  o += 8;
  const exponent = data.readInt32LE(o);
  o += 4;
  const publishTime = Number(data.readBigInt64LE(o));
  o += 8;
  o += 8; // prev_publish_time
  o += 8; // ema_price
  o += 8; // ema_conf
  const postedSlot = Number(data.readBigUInt64LE(o));
  return {
    feedId,
    price,
    confidence,
    exponent,
    publishTime,
    postedSlot,
    verificationLevel: level === 0 ? "partial" : "full",
  };
}

export interface Age {
  seconds: number;
  slots: number;
  view: PriceView;
}

/**
 * How stale the pinned account is, measured against CLUSTER time.
 *
 * Never `Date.now()`. The guards compare against the on-chain clock, and a VPS
 * whose clock has drifted by a minute would mis-size every push decision against
 * a window that is only tens of seconds wide.
 */
export async function readPriceAge(
  connection: Connection,
  priceUpdate: PublicKey,
  now: { unix: number; slot: number },
): Promise<Age | null> {
  const info = await connection.getAccountInfo(priceUpdate, "confirmed");
  if (!info) return null;
  const view = decodePriceUpdate(info.data);
  return {
    seconds: now.unix - view.publishTime,
    slots: now.slot - view.postedSlot,
    view,
  };
}

export interface GuardWindow {
  maxAgeSeconds: number;
  maxAgeSlots: number;
}

/**
 * Should we carry a price update with this liquidation?
 *
 * Margins are subtracted because the transaction has to still be inside the
 * window when it LANDS, not when it was built. On devnet, measured at ~6 slots
 * per second, a 300-slot bound is about 50 seconds — the slot bound binds long
 * before the seconds bound, which is the opposite of what the guard defaults
 * suggest if you assume 400ms slots.
 */
export function needsPush(age: Age | null, guards: GuardWindow): boolean {
  if (age === null) return true;
  return age.seconds > guards.maxAgeSeconds - 10 || age.slots > guards.maxAgeSlots - 20;
}

export class OracleUpdater {
  private readonly hermes = new HermesClient(HERMES_URL, {});
  private readonly receiver: PythSolanaReceiver;

  constructor(connection: Connection, wallet: anchor.Wallet) {
    this.receiver = new PythSolanaReceiver({ connection, wallet });
  }

  /** The account a given feed's shard-0 update lands in. */
  accountFor(feedIdHex: string): PublicKey {
    return this.receiver.getPriceFeedAccountAddress(0, feedIdHex);
  }

  /**
   * Instructions that write a fresh price into the pinned account.
   *
   * Returns null when Hermes has nothing strictly newer than what is already on
   * chain. That matters: pyth-push-oracle rejects a non-newer update, and since
   * the push rides in the same transaction as the liquidation, its failure would
   * take the liquidation down with it.
   */
  async buildPushInstructions(
    feedIdHex: string,
    onChainPublishTime: number | null,
  ): Promise<TransactionInstruction[] | null> {
    const updates = await this.hermes.getLatestPriceUpdates([feedIdHex], { encoding: "base64" });
    const data = updates?.binary?.data;
    if (!data || data.length === 0) return null;

    const parsed = updates.parsed?.[0];
    if (parsed && onChainPublishTime !== null) {
      const fresh = Number(parsed.price.publish_time);
      if (!(fresh > onChainPublishTime)) return null;
    }

    const { postInstructions } = await this.receiver.buildUpdatePriceFeedInstructions(data, 0);

    // Ephemeral signers would make this un-composable with our own signing, and
    // the atomic UpdatePriceFeed path is not supposed to need any. If one shows
    // up, the SDK took a different route than the one this design verified.
    for (const ix of postInstructions) {
      if (ix.signers && ix.signers.length > 0) {
        throw new Error(
          "pyth push returned ephemeral signers; the atomic UpdatePriceFeed path was expected. " +
            "Set SEPARATE_PUSH=1 to fall back to a prior transaction.",
        );
      }
    }
    return postInstructions.map((i) => i.instruction);
  }
}
