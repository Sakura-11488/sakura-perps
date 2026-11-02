import assert from "node:assert/strict";
import test from "node:test";
import {
  fetchHermesUpdate,
  HermesOptions,
  MAX_HERMES_RESPONSE_BYTES,
  MAX_HERMES_UPDATE_BYTES,
} from "../src/hermes";

const FEED = "ab".repeat(32);
const NOW = 1_800_000_000;
const AMOUNT = 12_345_000_000n;

// Structural fixture only: the dummy VAA has NO valid signatures. Acceptance by
// this transport parser never implies acceptance by the on-chain Pyth receiver.
function update(publishTime = NOW) {
  const message = Buffer.alloc(85);
  Buffer.from(FEED, "hex").copy(message, 1);
  message.writeBigInt64BE(AMOUNT, 33);
  message.writeBigUInt64BE(5n, 41);
  message.writeInt32BE(-8, 49);
  message.writeBigInt64BE(BigInt(publishTime), 53);
  const envelope = Buffer.concat([
    Buffer.from("504e4155010000000001", "hex"), // PNAU v1.0, no extension, proof0, VAA size1
    Buffer.from([1, 1, 0, 85]), // dummy VAA, one update, message size85
    message,
    Buffer.from([0]), // no proof nodes in this structural fixture
  ]);
  return {
    binary: { encoding: "base64", data: [envelope.toString("base64")] },
    parsed: [
      {
        id: FEED,
        price: { price: AMOUNT.toString(), conf: "5", expo: -8, publish_time: publishTime },
      },
    ],
  };
}

function options(response: () => Response | Promise<Response>): HermesOptions {
  return {
    endpoint: "https://hermes.invalid/prefix",
    nowSeconds: () => NOW,
    fetchImpl: async () => response(),
    timeoutMs: 500,
  };
}
function json(value: unknown): Response {
  return new Response(JSON.stringify(value), { headers: { "content-type": "application/json" } });
}

function mutateMessage(body: ReturnType<typeof update>, mutate: (message: Buffer) => void) {
  const bytes = Buffer.from(body.binary.data[0], "base64");
  mutate(bytes.subarray(14, 99));
  body.binary.data[0] = bytes.toString("base64");
}

test("bounded Hermes reader binds a single binary feed to matching JSON and never follows redirects", async () => {
  const body = update();
  let inspected = false;
  const result = await fetchHermesUpdate(`0x${FEED.toUpperCase()}`, {
    ...options(() => json(body)),
    authorization: "Bearer fake-test-credential",
    fetchImpl: async (input, init) => {
      const url = new URL(String(input));
      assert.equal(url.pathname, "/prefix/v2/updates/price/latest");
      assert.deepEqual(url.searchParams.getAll("ids[]"), [FEED]);
      assert.equal(url.searchParams.get("encoding"), "base64");
      assert.equal(url.searchParams.get("parsed"), "true");
      assert.equal(init?.redirect, "error");
      assert.equal(new Headers(init?.headers).get("authorization"), "Bearer fake-test-credential");
      inspected = true;
      return json(body);
    },
  });
  assert.ok(inspected);
  assert.deepEqual(result, { data: body.binary.data, publishTime: NOW });
});

test("advertised and streamed response sizes are bounded before JSON parsing", async () => {
  await assert.rejects(
    fetchHermesUpdate(
      FEED,
      options(
        () =>
          new Response("{}", {
            headers: { "content-length": String(MAX_HERMES_RESPONSE_BYTES + 1) },
          }),
      ),
    ),
    /size limit/,
  );
  let cancelled = false;
  let chunks = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      chunks++;
      controller.enqueue(new Uint8Array(16 * 1024));
    },
    cancel() {
      cancelled = true;
    },
  });
  await assert.rejects(
    fetchHermesUpdate(
      FEED,
      options(() => new Response(stream)),
    ),
    /size limit/,
  );
  assert.ok(cancelled, "oversized upstream stream must be cancelled");
  assert.ok(chunks <= 6, "the reader must stop consuming at the size bound");
});

test("malformed, extra, noncanonical and oversized binary payloads fail closed", async () => {
  const cases: Array<(b: ReturnType<typeof update>) => void> = [
    (b) => {
      b.binary.encoding = "hex";
    },
    (b) => {
      b.binary.data = [];
    },
    (b) => {
      b.binary.data.push(b.binary.data[0]);
    },
    (b) => {
      b.parsed = [];
    },
    (b) => {
      b.parsed.push(b.parsed[0]);
    },
    (b) => {
      b.binary.data[0] += "\n";
    },
    (b) => {
      b.binary.data[0] = "%%%%";
    },
    (b) => {
      b.binary.data[0] = Buffer.alloc(MAX_HERMES_UPDATE_BYTES + 1).toString("base64");
    },
    (b) => {
      b.binary.data[0] = Buffer.from("PNAU").toString("base64");
    },
    (b) => {
      b.binary.data[0] = Buffer.concat([
        Buffer.from(b.binary.data[0], "base64"),
        Buffer.from([0]),
      ]).toString("base64");
    },
    (b) => {
      mutateMessage(b, (m) => {
        m[0] = 1;
      });
    },
  ];
  for (const mutate of cases) {
    const body = update();
    mutate(body);
    await assert.rejects(
      fetchHermesUpdate(
        FEED,
        options(() => json(body)),
      ),
      /Hermes/,
    );
  }
  await assert.rejects(
    fetchHermesUpdate(
      FEED,
      options(() => new Response("{not JSON")),
    ),
    /validation failed/,
  );
});

test("JSON cannot relabel another binary feed, price, confidence, exponent or timestamp", async () => {
  const cases: Array<(b: ReturnType<typeof update>) => void> = [
    (b) => {
      b.parsed[0].id = "cd".repeat(32);
    },
    (b) => {
      mutateMessage(b, (m) => {
        m[1] ^= 1;
      });
    },
    (b) => {
      b.parsed[0].price.price = "1";
    },
    (b) => {
      b.parsed[0].price.conf = "6";
    },
    (b) => {
      b.parsed[0].price.expo = -6;
    },
    (b) => {
      b.parsed[0].price.publish_time = NOW - 1;
    },
    (b) => {
      mutateMessage(b, (m) => {
        m.writeBigInt64BE(-1n, 33);
      });
    },
    (b) => {
      b.parsed[0].price.price = "9".repeat(100);
    },
    (b) => {
      b.parsed[0].price.conf = "-1";
    },
  ];
  for (const mutate of cases) {
    const body = update();
    mutate(body);
    await assert.rejects(
      fetchHermesUpdate(
        FEED,
        options(() => json(body)),
      ),
      /invalid price update/,
    );
  }
});

test("embedded binary timestamps must be safe, recent and no more than five seconds ahead", async () => {
  for (const time of [NOW - 121, NOW + 6, 0, Number.MAX_SAFE_INTEGER + 1]) {
    await assert.rejects(
      fetchHermesUpdate(
        FEED,
        options(() => json(update(time))),
      ),
      /invalid price update/,
    );
  }
  for (const time of [NOW - 120, NOW + 5]) {
    assert.equal(
      (
        await fetchHermesUpdate(
          FEED,
          options(() => json(update(time))),
        )
      ).publishTime,
      time,
    );
  }
});

test("freshness is checked again at receipt using cluster time, including clock regression", async () => {
  for (const completedAt of [NOW + 121, NOW - 1]) {
    let clock = NOW;
    await assert.rejects(
      fetchHermesUpdate(FEED, {
        ...options(() => {
          clock = completedAt;
          return json(update());
        }),
        nowSeconds: () => clock,
      }),
      /invalid price update|Invalid Hermes request clock/,
    );
  }
});

test("request and stalled-body deadlines abort and return without awaiting upstream", async () => {
  let aborted = false;
  await assert.rejects(
    fetchHermesUpdate(FEED, {
      ...options(() => json(update())),
      timeoutMs: 15,
      fetchImpl: async (_input, init) => {
        init?.signal?.addEventListener("abort", () => {
          aborted = true;
        });
        return await new Promise<Response>(() => {});
      },
    }),
    /timed out/,
  );
  assert.ok(aborted);
  let cancelled = false;
  const stalled = new ReadableStream<Uint8Array>({
    cancel() {
      cancelled = true;
    },
  });
  await assert.rejects(
    fetchHermesUpdate(FEED, {
      ...options(() => new Response(stalled)),
      timeoutMs: 15,
    }),
    /timed out/,
  );
  assert.ok(cancelled);
});

test("network and HTTP errors never echo endpoint secrets, headers or response bodies", async () => {
  const secret = "fake-test-only-secret";
  for (const response of [
    () => new Response(secret, { status: 401 }),
    () => {
      throw new Error(`upstream https://host.invalid/${secret} Bearer ${secret}`);
    },
  ]) {
    await assert.rejects(
      fetchHermesUpdate(FEED, {
        ...options(response),
        endpoint: `https://host.invalid/${secret}?token=${secret}`,
        authorization: `Bearer ${secret}`,
      }),
      (error) =>
        error instanceof Error &&
        error.message.startsWith("Hermes") &&
        !String(error.stack).includes(secret),
    );
  }
});

test("invalid feed and endpoint configurations fail before making a request", async () => {
  let calls = 0;
  const opts = options(() => {
    calls++;
    return json(update());
  });
  for (const feed of ["", "11", `${FEED}&ids[]=other`, "zz".repeat(32)]) {
    await assert.rejects(fetchHermesUpdate(feed, opts), /Invalid Hermes feed/);
  }
  for (const endpoint of [
    "not a URL",
    "http://host.invalid",
    "https://name:password@host.invalid",
  ]) {
    await assert.rejects(fetchHermesUpdate(FEED, { ...opts, endpoint }), /Invalid Hermes endpoint/);
  }
  assert.equal(calls, 0);
});
