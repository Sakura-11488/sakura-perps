import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test, mock } from "node:test";
import anchor from "@coral-xyz/anchor";
import { Connection, PublicKey, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";

const SECRET = "SENTINEL_PRIVATE_RPC_CREDENTIAL_4895";
process.env.RPC = `https://user:${SECRET}@rpc.invalid/${SECRET}?api-key=${SECRET}`;
const { readOracleState, readChainState } = await import("../lib/chain.js");
const { currentOracle } = await import("../lib/observation.js");
const { readResponse } = await import("../lib/api-response.js");
const { GET: getPrice } = await import("../app/api/price/route.js");
const { GET: getState } = await import("../app/api/state/route.js");
const RECEIVER = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const KEY = PublicKey.default;
const TIME = 1_800_000_000n;
const SLOT = 500n;
const failure = async () => { throw new Error(`RPC failed: ${process.env.RPC} Authorization: ${SECRET}`); };

function oracleInfo() {
  const data = Buffer.alloc(134);
  createHash("sha256").update("account:PriceUpdateV2").digest().copy(data, 0, 0, 8);
  data[40] = 1;
  Buffer.from("ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d", "hex").copy(data, 41);
  data.writeBigInt64LE(12_345_000_000n, 73);
  data.writeBigUInt64LE(1_000_000n, 81);
  data.writeInt32LE(-8, 89);
  data.writeBigInt64LE(TIME, 93);
  data.writeBigUInt64LE(SLOT, 125);
  return { owner: RECEIVER, executable: false, data };
}

function connection(info = oracleInfo(), clock = true) {
  return {
    async getAccountInfo(key) {
      if (key.equals(SYSVAR_CLOCK_PUBKEY)) {
        if (!clock) return failure();
        const data = Buffer.alloc(40);
        data.writeBigUInt64LE(SLOT, 0);
        data.writeBigInt64LE(TIME + 1n, 32);
        return { data };
      }
      return info;
    },
    async getSignaturesForAddress() { return []; },
  };
}

function program(fail = false, pausedFlags = new anchor.BN(0)) {
  return {
    programId: KEY, methods: { initialize: true },
    account: {
      exchange: { fetch: fail ? failure : async () => ({
        admin: KEY, feeRecipient: KEY, collateralMint: KEY, collateralDecimals: 6,
        collateralFreezeAuthority: KEY, pausedFlags, protocolFeeShareBps: 1, numMarkets: 0,
      }) },
      pool: { all: fail ? failure : async () => [] },
      withdrawRequest: { all: fail ? failure : async () => [] },
    },
  };
}

function noSecrets(value) {
  const json = JSON.stringify(value);
  assert.ok(!json.includes(SECRET), "RPC credential must not leave the server");
  assert.ok(!json.includes("rpc.invalid"), "RPC endpoint must not leave the server");
  assert.ok(!json.includes("Authorization:"), "provider errors must not leave the server");
}

test("successful full snapshot contains only public state, never the configured RPC", async () => {
  const state = await readChainState({ program: program(), connection: connection() });
  assert.equal(state.status, "available");
  assert.equal(state.oracle.price, 123.45);
  assert.equal(Object.hasOwn(state, "rpc"), false);
  noSecrets(state);
});

test("real Anchor BN pause flags distinguish unpaused, partly paused, and fully paused", async () => {
  for (const flags of [0, 1, 63]) {
    const state = await readChainState({
      program: program(false, new anchor.BN(flags)), connection: connection(),
    });
    assert.equal(state.exchange.paused, flags !== 0, `pause flags ${flags}`);
    assert.equal(state.status, "available");
  }
});

test("pool utilization follows reserved liabilities rather than trader collateral", async () => {
  for (const [reservedUsdc, collateralUsdc] of [[20, 2], [10, 80]]) {
    const p = program();
    p.account.pool.all = async () => [{ publicKey: KEY, account: {
      quoteVault: KEY, shareMint: KEY, totalShares: new anchor.BN(100_000_000),
      quoteDeposited: new anchor.BN(100_000_000),
      reservedQuote: new anchor.BN(reservedUsdc * 1_000_000),
      lockedQuote: new anchor.BN(collateralUsdc * 1_000_000),
      maxUtilizationBps: 2000, maxAumQuote: new anchor.BN(1_000_000_000),
      depositFeeBps: 0, withdrawFeeBps: 0, withdrawDelaySeconds: 0,
    } }];
    const conn = connection();
    conn.getTokenAccountBalance = async () => ({ value: { uiAmountString: "200" } });
    conn.getTokenSupply = async () => ({ value: { amount: "100000000" } });
    const state = await readChainState({ program: p, connection: conn });
    assert.equal(state.pools[0].utilizationPct, reservedUsdc);
    assert.equal(state.pools[0].reserved, reservedUsdc);
    assert.equal(state.pools[0].locked, collateralUsdc);
    assert.equal(state.pools[0].maxUtilizationPct, 20);
    assert.equal(state.status, "available");
  }
});

test("every failed state section returns a bounded code without the provider exception", async () => {
  const state = await readChainState({
    program: program(true),
    connection: { getAccountInfo: failure, getSignaturesForAddress: failure },
  });
  assert.equal(state.status, "degraded");
  for (const section of ["oracle", "exchange", "pools", "requests", "activity"]) {
    assert.equal(state[`${section}Error`], `${section}_unavailable`);
  }
  noSecrets(state);
});

test("route error and missing-account responses are unavailable and never cached", async () => {
  for (const read of [failure, async () => null]) {
    const response = await readResponse(read, "oracle_unavailable");
    assert.equal(response.status, 503);
    assert.equal(response.headers.get("cache-control"), "no-store");
    const body = await response.json();
    assert.deepEqual(body, { error: "oracle_unavailable" });
    noSecrets(body);
  }
});

test("actual API routes never reflect RPC failures or credentials", async () => {
  mock.method(Connection.prototype, "getAccountInfo", failure);
  mock.method(Connection.prototype, "getProgramAccounts", failure);
  mock.method(Connection.prototype, "getSignaturesForAddress", failure);
  try {
    const price = await getPrice();
    assert.equal(price.status, 503);
    assert.deepEqual(await price.json(), { error: "oracle_unavailable" });
    const state = await getState();
    assert.equal(state.headers.get("cache-control"), "no-store");
    const body = await state.json();
    assert.equal(body.status, "degraded");
    noSecrets(body);
  } finally {
    mock.restoreAll();
  }
});

test("passing reference guards still leave actual market acceptance unknown", async () => {
  const state = await readOracleState(connection());
  for (const mode of Object.values(state.modes)) {
    assert.equal(mode.status, "unknown");
    assert.equal(Object.hasOwn(mode, "wouldAccept"), false);
    assert.equal(mode.guards.filter((g) => g.pass === true).length, 6);
    assert.equal(mode.guards.at(-1).pass, null);
  }
  assert.equal(state.scope, "reference_defaults_only");
  // Local wall-clock drift must not change freshness: cluster time is the source.
  assert.equal(state.ageSeconds, 1);
});

test("an unavailable cluster clock never passes the freshness or slot checks", async () => {
  const state = await readOracleState(connection(oracleInfo(), false));
  assert.equal(state.clockAvailable, false);
  assert.equal(state.ageSeconds, null);
  assert.equal(state.slotsAgo, null);
  for (const mode of Object.values(state.modes)) {
    assert.equal(mode.status, "unknown");
    assert.equal(mode.guards.find((g) => g.label === "On-chain slot age").pass, null);
    assert.equal(mode.guards.find((g) => g.label === "Upstream freshness").pass, null);
  }
  noSecrets(state);
});

test("future slots and timestamps fail reference guards", async () => {
  for (const change of [
    (d) => d.writeBigUInt64LE(SLOT + 1n, 125),
    (d) => d.writeBigInt64LE(TIME + 10n, 93),
  ]) {
    const info = oracleInfo(); change(info.data);
    const state = await readOracleState(connection(info));
    for (const mode of Object.values(state.modes)) assert.equal(mode.status, "failed");
  }
});

test("confidence and age reference boundaries are evaluated separately per mode", async () => {
  const info = oracleInfo();
  info.data.writeBigUInt64LE(246_900_000n, 81); // 2%, trading fails; liquidation default passes
  info.data.writeBigInt64LE(TIME - 65n, 93);
  const state = await readOracleState(connection(info));
  assert.equal(state.modes.trading.status, "failed");
  assert.equal(state.modes.liquidation.status, "unknown");
});

test("missing oracle accounts are unavailable, not an accepted price", async () => {
  assert.equal(await readOracleState(connection(null)), null);
  const state = await readChainState({ program: program(), connection: connection(null) });
  assert.equal(state.oracleError, "oracle_unavailable");
  assert.equal(state.status, "degraded");
});

test("malformed and unverified accounts cannot be presented as SOL/USD", async () => {
  const mutations = [
    (i) => { i.data = Buffer.alloc(132); },
    (i) => { i.data = Buffer.concat([i.data, Buffer.alloc(1)]); },
    (i) => { i.data[0] ^= 1; },
    (i) => { i.data[40] = 0; }, // Partial verification
    (i) => { i.data[40] = 2; }, // Invalid enum
    (i) => { i.owner = KEY; },
    (i) => { i.executable = true; },
    (i) => { i.data[41] ^= 1; }, // Different feed
    (i) => { i.data.writeInt32LE(-2147483648, 89); },
    (i) => { i.data.writeInt32LE(1, 89); },
    (i) => { i.data.writeBigInt64LE(2n ** 53n, 93); },
    (i) => { i.data.writeBigUInt64LE(2n ** 53n, 125); },
  ];
  for (const mutate of mutations) {
    const info = oracleInfo(); mutate(info);
    await assert.rejects(readOracleState(connection(info)), /invalid_oracle_data/);
    const response = await readResponse(() => readOracleState(connection(info)), "oracle_unavailable");
    assert.equal(response.status, 503);
    assert.deepEqual(await response.json(), { error: "oracle_unavailable" });
  }
});


test("the UI cannot reuse an old or unknown observation as a current guard verdict", () => {
  const now = 1_800_000_000;
  const fresh = { fetchedAt: new Date(now * 1000).toISOString(), price: 123 };
  const older = { fetchedAt: new Date((now - 5) * 1000).toISOString(), price: 122 };
  assert.equal(currentOracle({ oracle: older, snapshot: fresh, now }), fresh);
  assert.equal(currentOracle({ oracle: fresh, snapshot: older, unavailable: true, now }), null);
  assert.equal(currentOracle({ snapshot: fresh, now: now + 16 }), null);
  assert.equal(currentOracle({ snapshot: { price: 123 }, now }), null);
  assert.equal(currentOracle({ snapshot: fresh, now: now - 6 }), null);
});

test("unreadable escrow and balances remain unknown and degrade the full snapshot", async () => {
  const p = program();
  p.account.pool.all = async () => [{ publicKey: KEY, account: {
    quoteVault: KEY, shareMint: KEY, totalShares: 100n, quoteDeposited: 100n,
    lockedQuote: 0n, reservedQuote: 0n, maxUtilizationBps: 2000, maxAumQuote: 1000n,
    depositFeeBps: 0, withdrawFeeBps: 0, withdrawDelaySeconds: 0,
  } }];
  p.account.withdrawRequest.all = async () => [{ publicKey: KEY, account: {
    owner: KEY, shares: 10n, requestedAt: TIME,
  } }];
  const conn = connection();
  const original = conn.getAccountInfo;
  conn.getAccountInfo = async (key) => {
    if (key.toBase58() === "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE" || key.equals(SYSVAR_CLOCK_PUBKEY)) {
      return original(key);
    }
    return failure();
  };
  conn.getTokenAccountBalance = failure;
  conn.getTokenSupply = failure;
  const state = await readChainState({ program: p, connection: conn });
  assert.equal(state.status, "degraded");
  assert.equal(state.pools[0].vaultBalance, null);
  assert.equal(state.pools[0].shareSupply, null);
  assert.equal(state.requests[0].escrowExists, null);
  assert.equal(state.requests[0].escrowShares, null);
  noSecrets(state);
});
