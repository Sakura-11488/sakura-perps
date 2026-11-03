import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:http";
import { once } from "node:events";
import { Connection, PublicKey } from "@solana/web3.js";
import { createBoundedFetch, createBoundedRpcFetch, rpcLimitsFromEnv } from "../src/bounded-fetch";
import { createHeartbeat } from "../src/heartbeat";

const SECRET = "OFFLINE_TEST_SECRET_6494";
const ENDPOINT = `https://rpc.invalid/provider/${SECRET}?api-key=${SECRET}`;
const body = '{"jsonrpc":"2.0","id":"id1","result":{"context":{"slot":123},"value":17}}';
function noSecret(error: unknown): boolean {
  return (
    error instanceof Error &&
    error.message.startsWith("Network request failed (") &&
    !String(error.stack).includes(SECRET)
  );
}

test("RPC transport preserves exact body, request headers, IDs, context and simulation error data", async () => {
  const payload =
    '{ "jsonrpc":"2.0", "id":"custom", "result":{"context":{"slot":123},"value":{"err":{"InstructionError":[2,{"Custom":6062}]},"logs":["Program custom failed"]}},"error":{"code":-32002,"data":{"logs":["diagnostic"]}} }';
  const request =
    '{"jsonrpc":"2.0","id":"custom","method":"simulateTransaction","params":["bytes",{"minContextSlot":123}]}';
  const bounded = createBoundedRpcFetch({
    fetchImpl: async (input, init) => {
      assert.equal(String(input), ENDPOINT);
      assert.equal(init?.method, "POST");
      assert.equal(init?.body, request);
      assert.equal(new Headers(init?.headers).get("authorization"), `Bearer ${SECRET}`);
      assert.equal(init?.redirect, "error");
      return new Response(payload, {
        statusText: SECRET,
        headers: { "X-Provider-Secret": SECRET, "Content-Encoding": "gzip" },
      });
    },
  });
  const response = await bounded(ENDPOINT, {
    method: "POST",
    body: request,
    headers: { Authorization: `Bearer ${SECRET}` },
  });
  assert.equal(await response.text(), payload);
  assert.equal(response.statusText, "");
  assert.equal(response.headers.get("X-Provider-Secret"), null);
  assert.equal(response.headers.get("Content-Encoding"), null);
});

test("real web3 Connection preserves minContextSlot and same-bank response context through adapter", async () => {
  const connection = new Connection(ENDPOINT, {
    fetch: createBoundedRpcFetch({
      fetchImpl: async (_input, init) => {
        const request = JSON.parse(String(init?.body));
        assert.equal(request.method, "getBalance");
        assert.equal(request.params[1].minContextSlot, 123);
        return new Response(
          JSON.stringify({
            jsonrpc: "2.0",
            id: request.id,
            result: { context: { slot: 123 }, value: 456 },
          }),
        );
      },
    }),
  });
  assert.deepEqual(
    await connection.getBalanceAndContext(PublicKey.default, { minContextSlot: 123 }),
    { context: { slot: 123 }, value: 456 },
  );
});

test("RPC deadline includes stalled headers and stalled bodies, including uncooperative cancellation", async () => {
  let aborted = false;
  const stalledHeaders = createBoundedRpcFetch({
    timeoutMs: 15,
    fetchImpl: async (_input, init) => {
      init?.signal?.addEventListener("abort", () => {
        aborted = true;
      });
      return await new Promise<Response>(() => {});
    },
  });
  await assert.rejects(stalledHeaders(ENDPOINT), /timeout/);
  assert.ok(aborted);
  let cancelled = false;
  const stream = new ReadableStream<Uint8Array>({
    cancel() {
      cancelled = true;
      return new Promise(() => {});
    },
  });
  const stalledBody = createBoundedRpcFetch({
    timeoutMs: 15,
    fetchImpl: async () => new Response(stream),
  });
  await assert.rejects(stalledBody(ENDPOINT), /timeout/);
  assert.ok(cancelled);
});

test("caller abort is honored before dispatch and after headers without exposing abort reason", async () => {
  const already = new AbortController();
  already.abort(new Error(SECRET));
  let dispatched = false;
  const bounded = createBoundedFetch({
    fetchImpl: async () => {
      dispatched = true;
      return new Response(body);
    },
  });
  await assert.rejects(bounded(ENDPOINT, { signal: already.signal }), noSecret);
  assert.equal(dispatched, false);
  const current = new AbortController();
  let cancelled = false;
  const afterHeaders = createBoundedFetch({
    fetchImpl: async () => {
      setTimeout(() => current.abort(new Error(SECRET)), 5);
      return new Response(
        new ReadableStream({
          cancel() {
            cancelled = true;
          },
        }),
      );
    },
  });
  await assert.rejects(afterHeaders(ENDPOINT, { signal: current.signal }), noSecret);
  assert.ok(cancelled);
});

test("declared, streamed and empty-chunk response abuse is bounded", async () => {
  await assert.rejects(
    createBoundedRpcFetch({
      maxResponseBytes: 32,
      fetchImpl: async () => new Response("{}", { headers: { "content-length": "33" } }),
    })(ENDPOINT),
    /response_too_large/,
  );
  let cancelled = false;
  const stream = new ReadableStream<Uint8Array>({
    pull(c) {
      c.enqueue(new Uint8Array(16));
    },
    cancel() {
      cancelled = true;
    },
  });
  await assert.rejects(
    createBoundedRpcFetch({ maxResponseBytes: 32, fetchImpl: async () => new Response(stream) })(
      ENDPOINT,
    ),
    /response_too_large/,
  );
  assert.ok(cancelled);
  const empty = new ReadableStream<Uint8Array>({
    pull(c) {
      c.enqueue(new Uint8Array());
    },
  });
  await assert.rejects(
    createBoundedRpcFetch({ fetchImpl: async () => new Response(empty) })(ENDPOINT),
    /invalid_body/,
  );
});

test("large realistic account lists survive buffering without truncation", async () => {
  const payload = JSON.stringify({
    jsonrpc: "2.0",
    id: "accounts",
    result: Array.from({ length: 4_000 }, () => ({
      pubkey: PublicKey.default.toBase58(),
      account: {
        data: [Buffer.alloc(512).toString("base64"), "base64"],
        executable: false,
        lamports: 1,
        owner: PublicKey.default.toBase58(),
        rentEpoch: 1,
      },
    })),
  });
  assert.ok(payload.length > 3 * 1024 * 1024);
  let offset = 0;
  const bytes = new TextEncoder().encode(payload);
  const stream = new ReadableStream<Uint8Array>({
    pull(c) {
      if (offset === bytes.length) {
        c.close();
        return;
      }
      const end = Math.min(offset + 3_001, bytes.length);
      c.enqueue(bytes.subarray(offset, end));
      offset = end;
    },
  });
  const response = await createBoundedRpcFetch({ fetchImpl: async () => new Response(stream) })(
    ENDPOINT,
  );
  assert.equal(await response.text(), payload);
});

test("malformed JSON, extreme nesting and invalid UTF8 produce fixed errors", async () => {
  for (const input of [SECRET, "[".repeat(129) + "0" + "]".repeat(129), new Uint8Array([0xff])]) {
    await assert.rejects(
      createBoundedRpcFetch({ fetchImpl: async () => new Response(input) })(ENDPOINT),
      noSecret,
    );
  }
  const brackets = JSON.stringify({ result: '["quoted brackets"]'.repeat(200) });
  assert.equal(
    await (
      await createBoundedRpcFetch({ fetchImpl: async () => new Response(brackets) })(ENDPOINT)
    ).text(),
    brackets,
  );
});

test("HTTP failures and network exceptions are sanitized, while 429 retains safe retry semantics", async () => {
  for (const fetchImpl of [
    async () => new Response(SECRET, { status: 503, statusText: SECRET }),
    async () => {
      throw new Error(`Authorization: Bearer ${SECRET} ${ENDPOINT}`);
    },
  ])
    await assert.rejects(createBoundedRpcFetch({ fetchImpl })(ENDPOINT), noSecret);
  let cancelled = false;
  const response = await createBoundedRpcFetch({
    fetchImpl: async () =>
      new Response(
        new ReadableStream({
          cancel() {
            cancelled = true;
          },
        }),
        { status: 429, statusText: SECRET, headers: { Authorization: SECRET } },
      ),
  })(ENDPOINT);
  assert.equal(response.status, 429);
  assert.equal(response.statusText, "Too Many Requests");
  assert.equal(await response.text(), "");
  assert.equal(response.headers.get("Authorization"), null);
  assert.ok(cancelled);
});

test("transport limits reject invalid configuration and bound environment overrides", () => {
  assert.deepEqual(rpcLimitsFromEnv({}), { timeoutMs: 10_000, maxResponseBytes: 16 * 1024 * 1024 });
  for (const invalid of ["0", "-1", "NaN", "Infinity", "1.5", SECRET]) {
    assert.throws(() => rpcLimitsFromEnv({ RPC_TIMEOUT_MS: invalid }), noSecret);
    assert.throws(() => rpcLimitsFromEnv({ RPC_MAX_RESPONSE_BYTES: invalid }), noSecret);
  }
  assert.throws(() => rpcLimitsFromEnv({ RPC_TIMEOUT_MS: "30001" }), noSecret);
  assert.throws(
    () => rpcLimitsFromEnv({ RPC_MAX_RESPONSE_BYTES: String(64 * 1024 * 1024 + 1) }),
    noSecret,
  );
});

test("heartbeat timeout cannot hold the scan loop open and failed attempts remain rate limited", async () => {
  let failures = 0,
    requests = 0;
  const heartbeat = createHeartbeat({
    timeoutMs: 15,
    onFailure: () => {
      failures++;
    },
    fetchImpl: async () => {
      requests++;
      return await new Promise<Response>(() => {});
    },
  });
  await heartbeat(ENDPOINT, false, 1_000);
  assert.equal(requests, 0);
  await heartbeat(ENDPOINT, true, 1_000);
  assert.equal(failures, 1);
  await heartbeat(ENDPOINT, true, 60_999);
  assert.equal(requests, 1);
  await heartbeat(ENDPOINT, true, 61_000);
  assert.equal(requests, 2);
});

test("heartbeat body stall, oversize and non-2xx fail, and successful bodies are fully drained", async () => {
  for (const response of [
    () => new Response(new ReadableStream()),
    () => new Response("x".repeat(4_097)),
    () => new Response(SECRET, { status: 500 }),
  ]) {
    let failed = false;
    await createHeartbeat({
      timeoutMs: 15,
      fetchImpl: async () => response(),
      onFailure: () => {
        failed = true;
      },
    })(ENDPOINT, true, 1);
    assert.ok(failed);
  }
  let ended = false,
    failures = 0;
  const stream = new ReadableStream({
    start(c) {
      c.enqueue(new TextEncoder().encode("ok"));
    },
    pull(c) {
      ended = true;
      c.close();
    },
  });
  await createHeartbeat({
    fetchImpl: async () => new Response(stream),
    onFailure: () => {
      failures++;
    },
  })(ENDPOINT, true, 1);
  assert.ok(ended);
  assert.equal(failures, 0);
});

test("a reset heartbeat clock cannot suppress health pings behind an old throttle timestamp", async () => {
  let requests = 0;
  const heartbeat = createHeartbeat({
    fetchImpl: async () => {
      requests++;
      return new Response("ok");
    },
    onFailure: () => assert.fail("healthy response should not fail"),
  });
  await heartbeat(ENDPOINT, true, 100_000);
  await heartbeat(ENDPOINT, true, 100_001);
  assert.equal(requests, 1);
  await heartbeat(ENDPOINT, true, 5_000);
  assert.equal(requests, 2, "a reset clock establishes a new baseline immediately");
  await heartbeat(ENDPOINT, true, 64_999);
  assert.equal(requests, 2);
  await heartbeat(ENDPOINT, true, 65_000);
  assert.equal(requests, 3, "ordinary throttling resumes from the new baseline");
});

test("native HTTP redirects cannot forward a credential to another path", async () => {
  let forwarded = 0;
  const server = createServer((request, response) => {
    if (request.url?.startsWith("/target")) {
      forwarded++;
      response.end("{}");
    } else {
      response.writeHead(302, { Location: `/target?token=${SECRET}` });
      response.end(SECRET);
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  try {
    await assert.rejects(
      createBoundedRpcFetch()(`http://127.0.0.1:${address.port}/redirect`, {
        headers: { Authorization: `Bearer ${SECRET}` },
      }),
      noSecret,
    );
    assert.equal(forwarded, 0);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
  }
});
