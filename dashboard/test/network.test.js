import assert from "node:assert/strict";
import { test, mock } from "node:test";
import { PublicKey } from "@solana/web3.js";

const SECRET = "DASHBOARD_RPC_TEST_TOKEN_1144";
process.env.RPC = `https://rpc.invalid/${SECRET}?api-key=${SECRET}`;
process.env.RPC_TIMEOUT_MS = "500";
process.env.RPC_MAX_RESPONSE_BYTES = "65536";
const { readConnection } = await import("../lib/chain.js");
const { readResponse } = await import("../lib/api-response.js");

function clean(error) {
  return error instanceof Error && error.message.includes("Network request failed") && !String(error.stack).includes(SECRET);
}

test("dashboard real Connection uses bounded adapter while preserving context requests", async () => {
  mock.method(globalThis, "fetch", async (_input, init) => {
    const request = JSON.parse(init.body);
    assert.equal(request.method, "getBalance");
    assert.equal(request.params[1].minContextSlot, 67);
    assert.equal(init.redirect, "error");
    return new Response(JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { context: { slot: 67 }, value: 89 } }));
  });
  try {
    assert.deepEqual(await readConnection().getBalanceAndContext(PublicKey.default, { minContextSlot: 67 }), { context: { slot: 67 }, value: 89 });
  } finally { mock.restoreAll(); }
});

test("dashboard body timeout reaches a bounded unavailable response and cancels upstream", async () => {
  process.env.RPC_TIMEOUT_MS = "20";
  let cancelled = false;
  mock.method(globalThis, "fetch", async () => new Response(new ReadableStream({ cancel() { cancelled = true; } })));
  try {
    const response = await readResponse(() => readConnection().getBalance(PublicKey.default), "rpc_unavailable");
    assert.equal(response.status, 503);
    assert.deepEqual(await response.json(), { error: "rpc_unavailable" });
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.ok(cancelled);
  } finally { mock.restoreAll(); process.env.RPC_TIMEOUT_MS = "500"; }
});

test("dashboard rejects oversized and credential-bearing HTTP failures without leaking provider data", async () => {
  for (const response of [
    () => new Response("x".repeat(65537)),
    () => new Response(SECRET, { status: 500, statusText: SECRET }),
  ]) {
    mock.method(globalThis, "fetch", async () => response());
    try { await assert.rejects(readConnection().getBalance(PublicKey.default), clean); }
    finally { mock.restoreAll(); }
  }
});

test("dashboard network configuration cannot silently disable limits", () => {
  const previous = process.env.RPC_TIMEOUT_MS;
  process.env.RPC_TIMEOUT_MS = "0";
  try { assert.throws(() => readConnection(), /invalid_configuration/); }
  finally { process.env.RPC_TIMEOUT_MS = previous; }
});
