/** Stable circular traversal: RPC ordering and a healthy prefix cannot starve exits. */
export function scanBatch<T>(
  rows: T[],
  key: (row: T) => string,
  cursor: string | null,
  limit: number,
): T[] {
  const sorted = [...rows].sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0));
  const after = cursor === null ? 0 : sorted.findIndex((row) => key(row) > cursor);
  const start = after < 0 ? 0 : after;
  return [...sorted.slice(start), ...sorted.slice(0, start)].slice(0, limit);
}

/** Problems persist until their scope is observed successfully again. */
export class Health {
  private problems = new Map<string, string>();

  fail(scope: string, reason: string): void {
    this.problems.set(scope, reason);
  }

  recover(scope: string): void {
    this.problems.delete(scope);
  }

  retainPositions(keys: Set<string>): void {
    this.retain("position:", keys);
  }

  retain(prefix: string, keys: Set<string>): void {
    for (const scope of this.problems.keys()) {
      if (scope.startsWith(prefix) && !keys.has(scope.slice(prefix.length)))
        this.problems.delete(scope);
    }
  }

  get healthy(): boolean {
    return this.problems.size === 0;
  }

  get reasons(): Record<string, string> {
    return Object.fromEntries(this.problems);
  }
}

/** An unobserved or stale part of the book is not a healthy book. */
export class ScanCoverage {
  private observed = new Map<string, number>();

  record(position: string, nowMs: number): void {
    this.observed.set(position, nowMs);
  }

  missing(positions: Set<string>, nowMs: number, maxAgeMs: number): number {
    for (const key of this.observed.keys()) if (!positions.has(key)) this.observed.delete(key);
    let missing = 0;
    for (const key of positions) {
      const last = this.observed.get(key);
      if (last === undefined || nowMs < last || nowMs - last > maxAgeMs) missing++;
    }
    return missing;
  }
}

/** Logs and heartbeats share the same operator-state verdict, including empty books. */
export function applyOperatingState(
  health: Health,
  state: {
    halted: boolean;
    breakerTripped: boolean;
    budgetOverrun: boolean;
    budgetExhausted: boolean;
  },
): void {
  for (const [reason, active] of Object.entries(state)) {
    const scope = `operator:${reason}`;
    if (active) health.fail(scope, reason);
    else health.recover(scope);
  }
}
