import { createBoundedFetch } from "./bounded-fetch";

/** A monitoring service must never hold the next keeper scan open indefinitely. */
export function createHeartbeat(options: {
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
  onFailure: () => void;
}) {
  const boundedFetch = createBoundedFetch({
    fetchImpl: options.fetchImpl,
    timeoutMs: options.timeoutMs ?? 2_000,
    maxResponseBytes: 4_096,
  });
  let lastAttempt: number | undefined;
  return async (url: string | undefined, healthy: boolean, nowMs: number): Promise<void> => {
    if (!url || !healthy) return;
    // Production callers supply monotonic performance.now(). If a test or
    // embedded caller resets its clock, establish a new throttle baseline
    // instead of suppressing health pings until the old timestamp catches up.
    if (lastAttempt !== undefined && nowMs >= lastAttempt && nowMs - lastAttempt < 60_000) return;
    lastAttempt = nowMs;
    try {
      await boundedFetch(url, { method: "GET" });
    } catch {
      options.onFailure();
    }
  };
}
