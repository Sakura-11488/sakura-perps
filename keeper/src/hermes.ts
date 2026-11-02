/** A bounded single-feed Hermes reader. This parses structure, not signatures.
 * The Pyth receiver and our on-chain oracle guards remain the trust boundary.
 * Never log URLs, credentials, response bodies or upstream exception messages.
 */
import { parseAccumulatorUpdateData } from "@pythnetwork/price-service-sdk";

export const MAX_HERMES_RESPONSE_BYTES = 64 * 1024;
export const MAX_HERMES_UPDATE_BYTES = 16 * 1024;
const MAX_AGE_SECONDS = 120;
const MAX_FUTURE_SECONDS = 5;

export class HermesError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HermesError";
  }
}

export interface HermesOptions {
  endpoint: string;
  authorization?: string;
  /** Dependency injection for offline tests; not a network configuration flag. */
  fetchImpl?: typeof fetch;
  /** Cluster-clock estimate, optionally advanced by monotonic elapsed time. */
  nowSeconds: () => number;
  timeoutMs?: number;
}

export interface HermesUpdate {
  data: string[];
  publishTime: number;
}

function invalid(): never {
  throw new HermesError("Hermes returned an invalid price update");
}

function record(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) invalid();
  return value as Record<string, unknown>;
}

function integerString(value: unknown, unsigned = false): bigint {
  if (
    typeof value !== "string" ||
    value.length > 21 ||
    !(unsigned ? /^(0|[1-9][0-9]*)$/ : /^-?(0|[1-9][0-9]*)$/).test(value)
  )
    invalid();
  return BigInt(value);
}

/** Require the exact requested feed in both JSON metadata and the binary price
 * message. JSON freshness must never authorize a different binary timestamp.
 * The accumulator's Merkle proof and VAA signatures are verified on-chain.
 */
function decodeUpdate(value: unknown, feedId: string, now: number): HermesUpdate {
  const body = record(value);
  const binary = record(body.binary);
  if (
    binary.encoding !== "base64" ||
    !Array.isArray(binary.data) ||
    binary.data.length !== 1 ||
    !Array.isArray(body.parsed) ||
    body.parsed.length !== 1
  )
    invalid();
  const encoded = binary.data[0];
  if (
    typeof encoded !== "string" ||
    encoded.length === 0 ||
    encoded.length > Math.ceil(MAX_HERMES_UPDATE_BYTES / 3) * 4 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)
  )
    invalid();
  const bytes = Buffer.from(encoded, "base64");
  if (
    bytes.length > MAX_HERMES_UPDATE_BYTES ||
    bytes.toString("base64") !== encoded ||
    bytes.length < 11 ||
    bytes[6] !== 0 ||
    bytes[7] !== 0
  )
    invalid();
  const accumulator = parseAccumulatorUpdateData(bytes);
  if (accumulator.vaa.length === 0 || accumulator.updates.length !== 1) invalid();
  const message = accumulator.updates[0].message;
  if (
    message.length !== 85 ||
    message[0] !== 0 ||
    message.subarray(1, 33).toString("hex") !== feedId
  )
    invalid();
  const parsed = record(body.parsed[0]);
  if (parsed.id !== feedId) invalid();
  const price = record(parsed.price);
  const amount = integerString(price.price);
  const confidence = integerString(price.conf, true);
  const publishTime = price.publish_time;
  if (
    amount <= 0n ||
    amount !== message.readBigInt64BE(33) ||
    confidence !== message.readBigUInt64BE(41) ||
    typeof price.expo !== "number" ||
    !Number.isInteger(price.expo) ||
    price.expo < -18 ||
    price.expo > 0 ||
    price.expo !== message.readInt32BE(49) ||
    typeof publishTime !== "number" ||
    !Number.isSafeInteger(publishTime) ||
    publishTime <= 0 ||
    BigInt(publishTime) !== message.readBigInt64BE(53) ||
    publishTime > now + MAX_FUTURE_SECONDS ||
    publishTime < now - MAX_AGE_SECONDS
  )
    invalid();
  return { data: [encoded], publishTime };
}

export async function fetchHermesUpdate(
  feedIdHex: string,
  options: HermesOptions,
): Promise<HermesUpdate> {
  const feedId = feedIdHex.replace(/^0x/, "").toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(feedId)) throw new HermesError("Invalid Hermes feed ID");
  const now = options.nowSeconds();
  const timeoutMs = options.timeoutMs ?? 10_000;
  if (
    !Number.isSafeInteger(now) ||
    now <= 0 ||
    !Number.isInteger(timeoutMs) ||
    timeoutMs < 1 ||
    timeoutMs > 30_000
  ) {
    throw new HermesError("Invalid Hermes request configuration");
  }
  let url: URL;
  try {
    url = new URL(options.endpoint);
    if (url.protocol !== "https:" || url.username || url.password || url.hash) throw new Error();
    url.pathname = `${url.pathname.replace(/\/$/, "")}/v2/updates/price/latest`;
    url.searchParams.set("ids[]", feedId);
    url.searchParams.set("encoding", "base64");
    url.searchParams.set("parsed", "true");
  } catch {
    throw new HermesError("Invalid Hermes endpoint configuration");
  }
  const controller = new AbortController();
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new HermesError("Hermes request timed out"));
      controller.abort();
    }, timeoutMs);
  });
  try {
    const request = (async () => {
      const response = await (options.fetchImpl ?? fetch)(url, {
        headers: {
          Accept: "application/json",
          ...(options.authorization ? { Authorization: options.authorization } : {}),
        },
        redirect: "error",
        signal: controller.signal,
      });
      if (!response.ok) throw new HermesError(`Hermes returned HTTP ${response.status}`);
      const length = response.headers.get("content-length");
      if (
        length !== null &&
        (!/^[0-9]+$/.test(length) || Number(length) > MAX_HERMES_RESPONSE_BYTES)
      ) {
        throw new HermesError("Hermes response exceeds the size limit");
      }
      if (!response.body) invalid();
      reader = response.body.getReader();
      const chunks: Buffer[] = [];
      let total = 0;
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        total += value.byteLength;
        if (total > MAX_HERMES_RESPONSE_BYTES)
          throw new HermesError("Hermes response exceeds the size limit");
        chunks.push(Buffer.from(value));
      }
      const receivedAt = options.nowSeconds();
      if (!Number.isSafeInteger(receivedAt) || receivedAt < now) {
        throw new HermesError("Invalid Hermes request clock");
      }
      return decodeUpdate(
        JSON.parse(Buffer.concat(chunks, total).toString("utf8")),
        feedId,
        receivedAt,
      );
    })();
    return await Promise.race([request, deadline]);
  } catch (error) {
    if (error instanceof HermesError) throw error;
    throw new HermesError("Hermes request or price update validation failed");
  } finally {
    if (timer !== undefined) clearTimeout(timer);
    controller.abort();
    // Never await an upstream cancellation callback beyond the request deadline.
    void reader?.cancel().catch(() => undefined);
  }
}
