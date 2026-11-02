/** Fsync'd state is the authority for spending and ambiguous submissions. */
import fs from "fs";
import path from "path";

export interface PendingTransaction {
  position: string;
  signature: string;
  blockhash: string;
  lastValidBlockHeight: number;
  reservedLamports: number;
  submittedAt: number;
  day: string;
}

interface State {
  version: 1;
  identity: string;
  cursor: string | null;
  spent: Record<string, number>;
  pending: PendingTransaction[];
  attempts: number[];
  budgetOverrun: boolean;
}

export const dayOf = (unix: number): string => new Date(unix * 1000).toISOString().slice(0, 10);
function validDay(value: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const timestamp = Date.parse(`${value}T00:00:00.000Z`);
  return Number.isFinite(timestamp) && new Date(timestamp).toISOString().slice(0, 10) === value;
}
export function lamports(value: number): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("invalid lamport amount");
  return value;
}

function validPending(p: PendingTransaction): boolean {
  return (
    !!p &&
    [p.position, p.signature, p.blockhash, p.day].every(
      (s) => typeof s === "string" && s.length > 0,
    ) &&
    [p.lastValidBlockHeight, p.reservedLamports, p.submittedAt].every(
      (n) => Number.isSafeInteger(n) && n >= 0,
    ) &&
    p.submittedAt <= 253_402_300_799 &&
    validDay(p.day) &&
    p.day === dayOf(p.submittedAt)
  );
}

export class Journal {
  private state: State;
  private persistenceFailed = false;

  constructor(
    private readonly file: string,
    identity: string,
  ) {
    let value: unknown;
    try {
      value = JSON.parse(fs.readFileSync(file, "utf8"));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT")
        throw new Error(
          "keeper journal unreadable; refusing to reset spending or pending transactions",
        );
    }
    if (value === undefined) {
      this.state = {
        version: 1,
        identity,
        cursor: null,
        spent: {},
        pending: [],
        attempts: [],
        budgetOverrun: false,
      };
    } else {
      if (value === null || typeof value !== "object" || Array.isArray(value))
        throw new Error("keeper journal must be an object; refusing to reset it");
      const s = value as State;
      // Preserve the previous keeper's spending ledger when upgrading.
      const legacy = value as { day?: string; spentLamports?: number };
      if (
        s.version === undefined &&
        Object.keys(legacy).sort().join(",") === "day,spentLamports" &&
        typeof legacy.day === "string" &&
        (validDay(legacy.day) || (legacy.day === "" && legacy.spentLamports === 0)) &&
        Number.isSafeInteger(legacy.spentLamports) &&
        legacy.spentLamports! >= 0
      ) {
        this.state = {
          version: 1,
          identity,
          cursor: null,
          spent: legacy.day ? { [legacy.day]: legacy.spentLamports! } : {},
          pending: [],
          attempts: [],
          budgetOverrun: false,
        };
      } else {
        if (
          s.version !== 1 ||
          s.identity !== identity ||
          !(s.cursor === null || typeof s.cursor === "string") ||
          !s.spent ||
          typeof s.spent !== "object" ||
          Array.isArray(s.spent) ||
          !Object.keys(s.spent).every(validDay) ||
          !Object.values(s.spent).every((n) => Number.isSafeInteger(n) && n >= 0) ||
          !Array.isArray(s.pending) ||
          !s.pending.every(validPending) ||
          !Number.isSafeInteger(s.pending.reduce((sum, p) => sum + p.reservedLamports, 0)) ||
          !Array.isArray(s.attempts) ||
          !s.attempts.every((n) => Number.isSafeInteger(n) && n >= 0) ||
          typeof s.budgetOverrun !== "boolean" ||
          new Set(s.pending.map((p) => p.position)).size !== s.pending.length ||
          new Set(s.pending.map((p) => p.signature)).size !== s.pending.length
        ) {
          throw new Error(
            "keeper journal invalid or belongs to another wallet/cluster; refusing to reset it",
          );
        }
        this.state = s;
      }
    }
    this.save();
  }

  private save(): void {
    if (this.persistenceFailed)
      throw new Error("keeper journal persistence failed; restart after repairing storage");
    try {
      const temporary = `${this.file}.tmp`;
      const fd = fs.openSync(temporary, "w", 0o600);
      try {
        fs.fchmodSync(fd, 0o600);
        fs.writeFileSync(fd, JSON.stringify(this.state));
        fs.fsyncSync(fd);
      } finally {
        fs.closeSync(fd);
      }
      fs.renameSync(temporary, this.file);
      const directory = fs.openSync(path.dirname(this.file), "r");
      try {
        fs.fsyncSync(directory);
      } finally {
        fs.closeSync(directory);
      }
    } catch (error) {
      this.persistenceFailed = true;
      throw error;
    }
  }

  get cursor(): string | null {
    return this.state.cursor;
  }
  advance(cursor: string): void {
    this.state.cursor = cursor;
    this.save();
  }
  get pending(): PendingTransaction[] {
    return this.state.pending.map((p) => ({ ...p }));
  }
  has(position: string): boolean {
    return this.state.pending.some((p) => p.position === position);
  }
  get overrun(): boolean {
    return this.state.budgetOverrun || this.persistenceFailed;
  }

  /** Outstanding transactions reserve budget even across midnight or a restart. */
  available(now: number, ceiling: number): number {
    lamports(ceiling);
    const reserved = this.state.pending.reduce((sum, p) => lamports(sum + p.reservedLamports), 0);
    const committed = lamports((this.state.spent[dayOf(now)] ?? 0) + reserved);
    return Math.max(0, ceiling - committed);
  }

  recentAttempts(now: number): number {
    // A clock regression must not clear attempts that look future-dated.
    return this.state.attempts.filter((t) => now - t < 3600).length;
  }

  reserve(p: Omit<PendingTransaction, "day">, dailyLimit: number, hourlyLimit: number): void {
    lamports(p.reservedLamports);
    if (this.has(p.position) || this.state.pending.some((x) => x.signature === p.signature))
      throw new Error("position or signature already pending");
    if (this.overrun || p.reservedLamports > this.available(p.submittedAt, dailyLimit))
      throw new Error("daily budget exhausted");
    if (this.recentAttempts(p.submittedAt) >= hourlyLimit)
      throw new Error("hourly send limit reached");
    const pending = { ...p, day: dayOf(p.submittedAt) };
    if (!validPending(pending)) throw new Error("invalid pending transaction");
    this.state.pending.push(pending);
    this.state.attempts = this.state.attempts.filter((t) => p.submittedAt - t < 3600);
    this.state.attempts.push(p.submittedAt);
    this.save(); // Must finish before any broadcast.
  }

  settle(signature: string, actualCost: number, settledAt: number): void {
    lamports(actualCost);
    const p = this.state.pending.find((x) => x.signature === signature);
    if (!p) throw new Error("unknown pending signature");
    const day = dayOf(settledAt);
    this.state.spent[day] = lamports((this.state.spent[day] ?? 0) + actualCost);
    // Surface any underestimate and stop spending until an operator investigates.
    this.state.budgetOverrun ||= actualCost > p.reservedLamports;
    this.state.pending = this.state.pending.filter((x) => x.signature !== signature);
    this.save();
  }
}

/** A wallet journal has one writer. Dead processes can restart; ambiguous locks fail closed. */
export function acquireLease(file: string): () => void {
  const lock = `${file}.lock`;
  const reclaim = `${lock}.reclaim`;
  if (fs.existsSync(reclaim))
    throw new Error(
      "keeper lock recovery in progress; inspect stale recovery locks before removing",
    );
  try {
    const fd = fs.openSync(lock, "wx", 0o600);
    try {
      fs.writeFileSync(fd, String(process.pid));
      fs.fsyncSync(fd);
    } finally {
      fs.closeSync(fd);
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    const pid = Number(fs.readFileSync(lock, "utf8"));
    if (!Number.isSafeInteger(pid) || pid <= 0)
      throw new Error("keeper lock is unreadable; inspect before removing it");
    try {
      process.kill(pid, 0);
    } catch (probe) {
      if ((probe as NodeJS.ErrnoException).code === "ESRCH") {
        // Serialize reapers so a second restart cannot unlink a newly owned lock.
        const recovery = fs.openSync(reclaim, "wx", 0o600);
        try {
          if (fs.readFileSync(lock, "utf8") !== String(pid))
            throw new Error("keeper lock changed during recovery");
          fs.unlinkSync(lock);
          const fd = fs.openSync(lock, "wx", 0o600);
          try {
            fs.writeFileSync(fd, String(process.pid));
            fs.fsyncSync(fd);
          } finally {
            fs.closeSync(fd);
          }
        } finally {
          fs.closeSync(recovery);
          fs.unlinkSync(reclaim);
        }
        return () => {
          if (fs.readFileSync(lock, "utf8") === String(process.pid)) fs.unlinkSync(lock);
        };
      }
      throw probe;
    }
    throw new Error("another keeper process owns this journal");
  }
  return () => {
    if (fs.readFileSync(lock, "utf8") === String(process.pid)) fs.unlinkSync(lock);
  };
}
