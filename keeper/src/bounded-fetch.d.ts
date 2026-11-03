import type { FetchFn } from "@solana/web3.js";

export class NetworkBoundaryError extends Error {
  readonly code: string;
}
export interface FetchLimits {
  timeoutMs?: number;
  maxResponseBytes?: number;
  fetchImpl?: typeof fetch;
}
export function createBoundedFetch(options?: FetchLimits): typeof fetch;
/** The SDK consumes standard Response.ok/status/statusText/text fields. */
export function createBoundedRpcFetch(options?: FetchLimits): FetchFn;
export function rpcLimitsFromEnv(
  env?: NodeJS.ProcessEnv,
): Required<Pick<FetchLimits, "timeoutMs" | "maxResponseBytes">>;
