// A failed fast poll must not revive an older full-state response. Keep the
// previous price history for the chart, but never present it as current state.
export function currentOracle({ oracle, snapshot, unavailable, now }) {
  if (unavailable) return null;
  const candidates = [oracle, snapshot].filter((value) =>
    value && Number.isFinite(Date.parse(value.fetchedAt)));
  candidates.sort((a, b) => Date.parse(b.fetchedAt) - Date.parse(a.fetchedAt));
  const latest = candidates[0];
  if (!latest) return null;
  const age = now - Date.parse(latest.fetchedAt) / 1000;
  return age < -5 || age > 15 ? null : latest;
}
