"use strict";

// Shared by the keeper and the read-only dashboard. Buffer complete responses
// inside the deadline: returning after headers would leave web3's res.text()
// unbounded. No endpoint, header, upstream exception or HTTP error body is logged.
const DEFAULT_RPC_TIMEOUT_MS = 10_000;
const DEFAULT_RPC_MAX_RESPONSE_BYTES = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 64 * 1024 * 1024;
const MAX_JSON_DEPTH = 128;

class NetworkBoundaryError extends Error {
  constructor(code) {
    super(`Network request failed (${code})`);
    this.name = "NetworkBoundaryError";
    this.code = code;
  }
}

function limit(value, fallback, maximum) {
  const candidate = value === undefined || value === "" ? fallback : Number(value);
  if (!Number.isSafeInteger(candidate) || candidate < 1 || candidate > maximum) {
    throw new NetworkBoundaryError("invalid_configuration");
  }
  return candidate;
}

function rpcLimitsFromEnv(env = process.env) {
  return {
    timeoutMs: limit(env.RPC_TIMEOUT_MS, DEFAULT_RPC_TIMEOUT_MS, 30_000),
    maxResponseBytes: limit(
      env.RPC_MAX_RESPONSE_BYTES,
      DEFAULT_RPC_MAX_RESPONSE_BYTES,
      MAX_RESPONSE_BYTES,
    ),
  };
}

function validateJson(text) {
  // Prevent deeply nested inputs from amplifying a bounded byte response into
  // an unbounded parser/object stack. Brackets inside strings do not count.
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (const ch of text) {
    if (inString) {
      if (escaped) escaped = false;
      else if (ch === "\\") escaped = true;
      else if (ch === '"') inString = false;
    } else if (ch === '"') inString = true;
    else if (ch === "{" || ch === "[") {
      if (++depth > MAX_JSON_DEPTH) throw new NetworkBoundaryError("invalid_json");
    } else if (ch === "}" || ch === "]") depth--;
  }
  try {
    JSON.parse(text);
  } catch {
    throw new NetworkBoundaryError("invalid_json");
  }
}

function createBoundedFetch(options = {}) {
  const timeoutMs = limit(options.timeoutMs, DEFAULT_RPC_TIMEOUT_MS, 30_000);
  const maxResponseBytes = limit(
    options.maxResponseBytes,
    DEFAULT_RPC_MAX_RESPONSE_BYTES,
    MAX_RESPONSE_BYTES,
  );
  const rpc = options.rpc === true;
  return async function boundedFetch(input, init = {}) {
    const expiresAt = performance.now() + timeoutMs;
    const controller = new AbortController();
    const callerSignal = init.signal ?? (typeof input === "object" ? input.signal : undefined);
    let reader;
    let timer;
    let failure;
    let rejectBoundary;
    const boundary = new Promise((_, reject) => {
      rejectBoundary = reject;
    });
    const stop = (code) => {
      if (failure) return;
      failure = new NetworkBoundaryError(code);
      rejectBoundary(failure);
      controller.abort();
      // A provider cannot hold the deadline open with its cancellation promise.
      void reader?.cancel().catch(() => undefined);
    };
    const onAbort = () => stop("aborted");
    const checkBoundary = () => {
      if (!failure && performance.now() >= expiresAt) stop("timeout");
      if (failure) throw failure;
    };
    try {
      callerSignal?.addEventListener("abort", onAbort, { once: true });
      if (callerSignal?.aborted) onAbort();
      timer = setTimeout(() => stop("timeout"), timeoutMs);
      const request = (async () => {
        if (failure) throw failure;
        const response = await (options.fetchImpl ?? globalThis.fetch)(input, {
          ...init,
          redirect: "error",
          signal: controller.signal,
        });
        // A fetch implementation that finishes after cancellation must not
        // start consuming another body after the caller already returned.
        if (!failure && performance.now() >= expiresAt) stop("timeout");
        if (failure) {
          void response.body?.cancel().catch(() => undefined);
          throw failure;
        }
        // Preserve web3's existing bounded rate-limit retry schedule while
        // never echoing provider status text, headers, or response bodies.
        if (rpc && response.status === 429) {
          void response.body?.cancel().catch(() => undefined);
          return new Response(null, { status: 429, statusText: "Too Many Requests" });
        }
        if (!response.ok) {
          void response.body?.cancel().catch(() => undefined);
          throw new NetworkBoundaryError("http_error");
        }
        const declared = response.headers.get("content-length");
        if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > maxResponseBytes)) {
          void response.body?.cancel().catch(() => undefined);
          throw new NetworkBoundaryError("response_too_large");
        }
        let bytes = Buffer.allocUnsafe(Math.min(64 * 1024, maxResponseBytes));
        let length = 0;
        let emptyChunks = 0;
        if (response.body) {
          reader = response.body.getReader();
          while (true) {
            const chunk = await reader.read();
            checkBoundary();
            if (chunk.done) break;
            if (chunk.value.byteLength === 0) {
              if (++emptyChunks > 1024) throw new NetworkBoundaryError("invalid_body");
              continue;
            }
            const required = length + chunk.value.byteLength;
            if (required > maxResponseBytes) throw new NetworkBoundaryError("response_too_large");
            if (required > bytes.length) {
              const grown = Buffer.allocUnsafe(
                Math.min(maxResponseBytes, Math.max(required, bytes.length * 2)),
              );
              bytes.copy(grown, 0, 0, length);
              bytes = grown;
            }
            bytes.set(chunk.value, length);
            length = required;
          }
        }
        const body = bytes.subarray(0, length);
        if (rpc) {
          const text = new TextDecoder("utf-8", { fatal: true }).decode(body);
          validateJson(text);
        }
        checkBoundary();
        // Preserve every successful JSON-RPC byte, including result/context and
        // structured error.data used by simulation classification. Only response
        // headers/statusText are replaced; no content encoding survives decoding.
        return new Response([204, 205, 304].includes(response.status) ? null : body, {
          status: response.status,
          headers: rpc ? { "content-type": "application/json" } : {},
        });
      })();
      return await Promise.race([request, boundary]);
    } catch (error) {
      if (failure) throw failure;
      if (error instanceof NetworkBoundaryError) throw error;
      throw new NetworkBoundaryError("transport_error");
    } finally {
      clearTimeout(timer);
      callerSignal?.removeEventListener("abort", onAbort);
      controller.abort();
      void reader?.cancel().catch(() => undefined);
    }
  };
}

function createBoundedRpcFetch(options = {}) {
  return createBoundedFetch({ ...options, rpc: true });
}

exports.NetworkBoundaryError = NetworkBoundaryError;
exports.createBoundedFetch = createBoundedFetch;
exports.createBoundedRpcFetch = createBoundedRpcFetch;
exports.rpcLimitsFromEnv = rpcLimitsFromEnv;
