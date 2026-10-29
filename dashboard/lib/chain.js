// Read-only reader for the sakura-perps devnet program.
// Server-side only. Sends nothing on-chain — pure account reads.
import { readFileSync } from "node:fs";
import path from "node:path";
import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";

const RPC = process.env.RPC ?? "https://api.devnet.solana.com";

// Reuse the IDL the repo already ships (../idl from this dashboard folder);
// fall back to a local copy so the app also runs standalone.
function loadIdl() {
  const candidates = [
    path.join(process.cwd(), "..", "idl", "sakura_perps.json"),
    path.join(process.cwd(), "idl", "sakura_perps.json"),
  ];
  for (const p of candidates) {
    try { return JSON.parse(readFileSync(p, "utf8")); } catch {}
  }
  throw new Error("sakura_perps.json IDL not found (looked in ../idl and ./idl)");
}
const idl = loadIdl();

// The live Pyth SOL/USD price update account the program is configured against.
const ORACLE_SOL_USD = new PublicKey(
  "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE",
);

const bn = (v) => (v ? BigInt(v.toString()) : 0n);
const usdc = (v) => Number(bn(v)) / 1e6;

function getProgram() {
  const connection = new Connection(RPC, "confirmed");
  const provider = new anchor.AnchorProvider(
    connection,
    new anchor.Wallet(Keypair.generate()), // throwaway; never signs
    { commitment: "confirmed" },
  );
  return { program: new anchor.Program(idl, provider), connection };
}

// Her oracle guards (crates/risk/src/oracle.rs). Trading is tight; liquidation
// is deliberately looser — refusing to liquidate is not a safe default.
const TRADING_GUARDS = {
  maxAgeSeconds: 60,
  maxAgeSlots: 150,
  maxFutureSkewSeconds: 5,
  maxConfidenceBps: 100,
  expectedExponent: -8, // Pyth crypto feeds
};
const LIQUIDATION_GUARDS = {
  maxAgeSeconds: 120,
  maxAgeSlots: 300,
  maxFutureSkewSeconds: 5,
  maxConfidenceBps: 500,
  expectedExponent: -8,
};

// Manually decode a Pyth PriceUpdateV2 account: 8-byte anchor discriminator,
// write_authority:32, verification_level enum, then the PriceFeedMessage and
// posted_slot.
function decodePythPrice(data) {
  let o = 8 + 32; // skip discriminator + write_authority
  const variant = data.readUInt8(o);
  o += 1;
  if (variant === 0) o += 1; // Partial { num_signatures: u8 }; Full has no payload
  const feedId = data.subarray(o, o + 32).toString("hex"); o += 32;
  const price = data.readBigInt64LE(o); o += 8;
  const conf = data.readBigUInt64LE(o); o += 8;
  const expo = data.readInt32LE(o); o += 4;
  const publishTime = data.readBigInt64LE(o); o += 8;
  o += 8; // prev_publish_time
  o += 8; // ema_price
  o += 8; // ema_conf
  const postedSlot = data.readBigUInt64LE(o); o += 8;
  const scale = 10 ** expo;
  const now = Math.floor(Date.now() / 1000);
  return {
    price: Number(price) * scale,
    confidence: Number(conf) * scale,
    exponent: expo,
    feedId,
    publishTime: Number(publishTime),
    postedSlot: Number(postedSlot),
    ageSeconds: Math.max(0, now - Number(publishTime)),
    futureSkewSeconds: Math.max(0, Number(publishTime) - now),
  };
}

// Evaluate the live price against a guard set, in her check order.
function evaluateGuards(d, currentSlot, G) {
  const confBps = d.price > 0 ? (d.confidence / d.price) * 10000 : Infinity;
  const slotsAgo = currentSlot != null ? currentSlot - d.postedSlot : null;
  return [
    { label: "Positive price", actual: `$${d.price.toFixed(2)}`, limit: "> 0",
      pass: d.price > 0, error: "OracleInvalidPrice" },
    { label: "Exponent matches", actual: `${d.exponent}`, limit: `= ${G.expectedExponent}`,
      pass: d.exponent === G.expectedExponent, error: "OracleExponentChanged" },
    { label: "Not future-dated", actual: `${d.futureSkewSeconds}s ahead`, limit: `≤ ${G.maxFutureSkewSeconds}s`,
      pass: d.futureSkewSeconds <= G.maxFutureSkewSeconds, error: "OraclePriceFromTheFuture" },
    { label: "Upstream freshness", actual: `${d.ageSeconds}s`, limit: `≤ ${G.maxAgeSeconds}s`,
      pass: d.ageSeconds <= G.maxAgeSeconds, error: "OracleStale" },
    { label: "On-chain slot age", actual: slotsAgo != null ? `${slotsAgo} slots` : "—", limit: `≤ ${G.maxAgeSlots}`,
      pass: slotsAgo != null ? slotsAgo <= G.maxAgeSlots : null, error: "OracleStale" },
    { label: "Confidence width", actual: `${confBps.toFixed(2)} bps`, limit: `≤ ${G.maxConfidenceBps} bps`,
      pass: confBps <= G.maxConfidenceBps, error: "OracleConfidenceTooWide" },
    { label: "Sanity band", actual: "per-market", limit: "unset (no market yet)",
      pass: null, error: "OraclePriceOutOfBand" },
  ];
}

// Lightweight oracle-only read (1-2 RPC calls) for fast polling.
export async function readOracleState(connection) {
  const conn = connection ?? new Connection(RPC, "confirmed");
  const info = await conn.getAccountInfo(ORACLE_SOL_USD);
  if (!info) return null;
  const d = decodePythPrice(info.data);
  let currentSlot = null;
  try { currentSlot = await conn.getSlot(); } catch {}
  const confidenceBps = d.price > 0 ? (d.confidence / d.price) * 10000 : null;
  const mkMode = (G) => {
    const guards = evaluateGuards(d, currentSlot, G);
    // Fails closed: any definite failure means the program would reject.
    return { guards, wouldAccept: guards.every((g) => g.pass !== false) };
  };
  return {
    feed: ORACLE_SOL_USD.toBase58(),
    ...d,
    confidenceBps,
    slotsAgo: currentSlot != null ? currentSlot - d.postedSlot : null,
    modes: {
      trading: mkMode(TRADING_GUARDS),
      liquidation: mkMode(LIQUIDATION_GUARDS),
    },
  };
}

export async function readChainState() {
  const { program, connection } = getProgram();
  const pid = program.programId;

  const [exchangePda] = PublicKey.findProgramAddressSync(
    [Buffer.from("exchange")], pid,
  );

  const out = {
    rpc: RPC,
    programId: pid.toBase58(),
    cluster: "devnet",
    fetchedAt: new Date().toISOString(),
    instructions: Object.keys(program.methods),
    exchange: null,
    pools: [],
    requests: [],
    oracle: null,
    activity: [],
  };

  // Live oracle price + guard evaluation against her trading thresholds.
  try {
    out.oracle = await readOracleState(connection);
  } catch (err) {
    out.oracleError = err.message;
  }

  try {
    const e = await program.account.exchange.fetch(exchangePda);
    out.exchange = {
      pda: exchangePda.toBase58(),
      admin: e.admin.toBase58(),
      feeRecipient: e.feeRecipient.toBase58(),
      collateralMint: e.collateralMint.toBase58(),
      collateralDecimals: e.collateralDecimals,
      freezeAuthority: e.collateralFreezeAuthority.toBase58(),
      isFreezable: !e.collateralFreezeAuthority.equals(PublicKey.default),
      paused: Buffer.from(e.pausedFlags).some((b) => b !== 0),
      protocolFeeBps: e.protocolFeeShareBps,
      numMarkets: e.numMarkets,
    };
  } catch (err) {
    out.exchangeError = err.message;
  }

  try {
    const pools = await program.account.pool.all();
    for (const p of pools) {
      const a = p.account;
      let vaultUsdc = null, shareSupply = null;
      try {
        vaultUsdc = (await connection.getTokenAccountBalance(a.quoteVault)).value.uiAmountString;
      } catch {}
      try {
        shareSupply = (await connection.getTokenSupply(a.shareMint)).value.amount;
      } catch {}
      const deposited = usdc(a.quoteDeposited);
      const locked = usdc(a.lockedQuote);
      out.pools.push({
        pubkey: p.publicKey.toBase58(),
        shareMint: a.shareMint.toBase58(),
        quoteVault: a.quoteVault.toBase58(),
        totalShares: bn(a.totalShares).toString(),
        shareSupply,
        deposited,
        locked,
        utilizationPct: deposited > 0 ? (locked / deposited) * 100 : 0,
        vaultBalance: vaultUsdc,
        maxUtilizationPct: a.maxUtilizationBps / 100,
        maxAum: usdc(a.maxAumQuote),
        depositFeeBps: a.depositFeeBps,
        withdrawFeeBps: a.withdrawFeeBps,
        withdrawDelaySeconds: a.withdrawDelaySeconds,
      });
    }
  } catch (err) {
    out.poolsError = err.message;
  }

  try {
    const reqs = await program.account.withdrawRequest.all();
    for (const r of reqs) {
      const a = r.account;
      const [escrowPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("withdraw_escrow"), a.owner.toBuffer()], pid,
      );
      let escrowShares = null, escrowExists = false;
      try {
        const info = await connection.getAccountInfo(escrowPda);
        escrowExists = !!info;
        if (info) escrowShares = (await connection.getTokenAccountBalance(escrowPda)).value.amount;
      } catch {}
      out.requests.push({
        pubkey: r.publicKey.toBase58(),
        owner: a.owner.toBase58(),
        shares: bn(a.shares).toString(),
        requestedAt: new Date(Number(bn(a.requestedAt)) * 1000).toISOString(),
        escrow: escrowPda.toBase58(),
        escrowExists,
        escrowShares,
      });
    }
  } catch (err) {
    out.requestsError = err.message;
  }

  // Recent program activity.
  try {
    const sigs = await connection.getSignaturesForAddress(pid, { limit: 8 });
    out.activity = sigs.map((s) => ({
      signature: s.signature,
      slot: s.slot,
      blockTime: s.blockTime,
      err: !!s.err,
    }));
  } catch (err) {
    out.activityError = err.message;
  }

  return out;
}
