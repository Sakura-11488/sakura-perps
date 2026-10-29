// Read-only reader for the sakura-perps devnet program.
// Server-side only. Sends nothing on-chain — pure account reads.
import "server-only";
import idl from "../../idl/sakura_perps.json" with { type: "json" };
import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";

const RPC = process.env.RPC ?? "https://api.devnet.solana.com";

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

// These are reference defaults only, not the live market's QualifiedFeed guards.
// Never infer program acceptance from this oracle-only monitor.
const TRADING_GUARDS = {
  maxAgeSeconds: 60,
  maxAgeSlots: 150,
  maxFutureSkewSeconds: 5,
  maxConfidenceBps: 100,
  expectedExponent: -8,
};
const LIQUIDATION_GUARDS = {
  ...TRADING_GUARDS,
  maxAgeSeconds: 120,
  maxAgeSlots: 300,
  maxConfidenceBps: 500,
};
const PYTH_RECEIVER = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const SOL_USD_FEED = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
// sha256("account:PriceUpdateV2")[0..8], from the Pyth receiver's Anchor account.
const PRICE_UPDATE_DISCRIMINATOR = Buffer.from([34, 241, 35, 99, 157, 126, 244, 205]);

function safeInteger(value) {
  const result = Number(value);
  if (!Number.isSafeInteger(result) || result < 0) throw new Error("invalid_oracle_data");
  return result;
}

function decodePythPrice(info) {
  const data = info.data;
  // Full verification serializes to 133 bytes; allocated accounts can have an
  // extra byte for the larger Partial variant. Do not decode truncated layouts.
  if (!info.owner?.equals(PYTH_RECEIVER) || info.executable ||
      !Buffer.isBuffer(data) || (data.length !== 133 && data.length !== 134) ||
      !data.subarray(0, 8).equals(PRICE_UPDATE_DISCRIMINATOR) || data[40] !== 1) {
    throw new Error("invalid_oracle_data");
  }
  let o = 41;
  const feedId = data.subarray(o, o + 32).toString("hex"); o += 32;
  if (feedId !== SOL_USD_FEED) throw new Error("invalid_oracle_data");
  const mantissa = data.readBigInt64LE(o); o += 8;
  const conf = data.readBigUInt64LE(o); o += 8;
  const exponent = data.readInt32LE(o); o += 4;
  if (exponent < -18 || exponent > 0) throw new Error("invalid_oracle_data");
  const publishTime = safeInteger(data.readBigInt64LE(o)); o += 8;
  o += 24; // prev_publish_time, ema_price, ema_conf
  const postedSlot = safeInteger(data.readBigUInt64LE(o));
  return {
    mantissa, conf, exponent, feedId, publishTime, postedSlot,
    price: Number(mantissa) * 10 ** exponent,
    confidence: Number(conf) * 10 ** exponent,
  };
}

async function readClock(connection) {
  try {
    const info = await connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY, "confirmed");
    if (!info || info.data.length !== 40) return null;
    return {
      slot: safeInteger(info.data.readBigUInt64LE(0)),
      unix: safeInteger(info.data.readBigInt64LE(32)),
    };
  } catch {
    return null;
  }
}

function evaluateGuards(d, clock, G) {
  const confidenceBps = d.price > 0 ? (d.confidence / d.price) * 10000 : null;
  const slotsAgo = clock ? clock.slot - d.postedSlot : null;
  const age = clock ? Math.max(0, clock.unix - d.publishTime) : null;
  const futureSkew = clock ? Math.max(0, d.publishTime - clock.unix) : null;
  return [
    { label: "Positive price", actual: `$${d.price.toFixed(2)}`, limit: "> 0",
      pass: d.mantissa > 0n, error: "OracleInvalidPrice" },
    { label: "Exponent matches", actual: `${d.exponent}`, limit: `= ${G.expectedExponent}`,
      pass: d.exponent === G.expectedExponent, error: "OracleExponentChanged" },
    { label: "Not future-dated", actual: futureSkew === null ? "unknown" : `${futureSkew}s ahead`, limit: `≤ ${G.maxFutureSkewSeconds}s`,
      pass: futureSkew === null ? null : futureSkew <= G.maxFutureSkewSeconds, error: "OraclePriceFromTheFuture" },
    { label: "Upstream freshness", actual: age === null ? "unknown" : `${age}s`, limit: `≤ ${G.maxAgeSeconds}s`,
      pass: age === null ? null : age <= G.maxAgeSeconds, error: "OracleStale" },
    { label: "On-chain slot age", actual: slotsAgo === null ? "unknown" : `${slotsAgo} slots`, limit: `0…${G.maxAgeSlots}`,
      pass: slotsAgo === null ? null : slotsAgo >= 0 && slotsAgo <= G.maxAgeSlots,
      error: slotsAgo < 0 ? "OraclePriceFromTheFuture" : "OracleStale" },
    { label: "Confidence width", actual: confidenceBps === null ? "unknown" : `${confidenceBps.toFixed(2)} bps`, limit: `≤ ${G.maxConfidenceBps} bps`,
      pass: d.mantissa > 0n && d.conf * 10000n <= d.mantissa * BigInt(G.maxConfidenceBps), error: "OracleConfidenceTooWide" },
    { label: "Market guards and sanity band", actual: "not loaded", limit: "unknown",
      pass: null, error: "MarketConfigurationNotChecked" },
  ];
}

// Lightweight reference monitor. Account provenance is checked, but market
// configuration, revocation, pause, EMA divergence and margin are not evaluated.
export async function readOracleState(connection) {
  const conn = connection ?? new Connection(RPC, "confirmed");
  const info = await conn.getAccountInfo(ORACLE_SOL_USD, "confirmed");
  if (!info) return null;
  const d = decodePythPrice(info);
  const clock = await readClock(conn);
  const mkMode = (G) => {
    const guards = evaluateGuards(d, clock, G);
    return {
      guards,
      status: guards.some((g) => g.pass === false) ? "failed" : "unknown",
    };
  };
  const { mantissa, conf, ...display } = d;
  return {
    feed: ORACLE_SOL_USD.toBase58(),
    ...display,
    fetchedAt: new Date().toISOString(),
    scope: "reference_defaults_only",
    clockAvailable: clock !== null,
    confidenceBps: d.price > 0 ? (d.confidence / d.price) * 10000 : null,
    ageSeconds: clock ? Math.max(0, clock.unix - d.publishTime) : null,
    futureSkewSeconds: clock ? Math.max(0, d.publishTime - clock.unix) : null,
    slotsAgo: clock ? clock.slot - d.postedSlot : null,
    modes: {
      trading: mkMode(TRADING_GUARDS),
      liquidation: mkMode(LIQUIDATION_GUARDS),
    },
  };
}

export async function readChainState(context) {
  const { program, connection } = context ?? getProgram();
  const pid = program.programId;

  const [exchangePda] = PublicKey.findProgramAddressSync(
    [Buffer.from("exchange")], pid,
  );

  const out = {
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

  // Live oracle price and reference checks; never a program-acceptance verdict.
  try {
    out.oracle = await readOracleState(connection);
    if (!out.oracle) out.oracleError = "oracle_unavailable";
  } catch {
    out.oracleError = "oracle_unavailable";
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
      paused: bn(e.pausedFlags) !== 0n,
      protocolFeeBps: e.protocolFeeShareBps,
      numMarkets: e.numMarkets,
    };
  } catch {
    out.exchangeError = "exchange_unavailable";
  }

  try {
    const pools = await program.account.pool.all();
    for (const p of pools) {
      const a = p.account;
      let vaultUsdc = null, shareSupply = null;
      try {
        vaultUsdc = (await connection.getTokenAccountBalance(a.quoteVault)).value.uiAmountString;
      } catch {
        out.poolsError = "pool_balances_unavailable";
      }
      try {
        shareSupply = (await connection.getTokenSupply(a.shareMint)).value.amount;
      } catch {
        out.poolsError = "pool_balances_unavailable";
      }
      const deposited = usdc(a.quoteDeposited);
      const locked = usdc(a.lockedQuote);
      const reserved = usdc(a.reservedQuote);
      out.pools.push({
        pubkey: p.publicKey.toBase58(),
        shareMint: a.shareMint.toBase58(),
        quoteVault: a.quoteVault.toBase58(),
        totalShares: bn(a.totalShares).toString(),
        shareSupply,
        deposited,
        locked,
        reserved,
        // The on-chain utilization cap covers promised payouts, not the
        // collateral traders deposited into lockedQuote.
        utilizationPct: deposited > 0 ? (reserved / deposited) * 100 : 0,
        vaultBalance: vaultUsdc,
        maxUtilizationPct: a.maxUtilizationBps / 100,
        maxAum: usdc(a.maxAumQuote),
        depositFeeBps: a.depositFeeBps,
        withdrawFeeBps: a.withdrawFeeBps,
        withdrawDelaySeconds: a.withdrawDelaySeconds,
      });
    }
  } catch {
    out.poolsError = "pools_unavailable";
  }

  try {
    const reqs = await program.account.withdrawRequest.all();
    for (const r of reqs) {
      const a = r.account;
      const [escrowPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("withdraw_escrow"), a.owner.toBuffer()], pid,
      );
      let escrowShares = null, escrowExists = null;
      try {
        const info = await connection.getAccountInfo(escrowPda);
        escrowExists = !!info;
        if (info) escrowShares = (await connection.getTokenAccountBalance(escrowPda)).value.amount;
      } catch {
        out.requestsError = "escrow_unavailable";
      }
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
  } catch {
    out.requestsError = "requests_unavailable";
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
  } catch {
    out.activityError = "activity_unavailable";
  }

  out.status = !out.oracle?.clockAvailable || Object.keys(out).some((key) => key.endsWith("Error"))
    ? "degraded" : "available";
  return out;
}
