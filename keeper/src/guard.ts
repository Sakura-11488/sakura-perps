/**
 * The rails that stop an unattended bot doing damage overnight.
 *
 * Every one of these exists for a specific failure, named in its comment. A rail
 * without a failure it prevents is decoration, and gets deleted.
 */
import fs from "fs";
import path from "path";
import { Connection, PublicKey } from "@solana/web3.js";
import type { Config } from "./config";

const stateDir = path.resolve(__dirname, "..");
const STATE = path.join(stateDir, ".state.json");
const HALT = path.join(stateDir, "HALT");

interface State {
  day: string;
  spentLamports: number;
}

const utcDay = (unix: number): string => new Date(unix * 1000).toISOString().slice(0, 10);

function readState(): State {
  try {
    return JSON.parse(fs.readFileSync(STATE, "utf8")) as State;
  } catch {
    return { day: "", spentLamports: 0 };
  }
}

/** tmp+rename, so a crash mid-write cannot leave an unparseable ledger. */
function writeState(s: State): void {
  const tmp = `${STATE}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(s));
  fs.renameSync(tmp, STATE);
}

export class Guard {
  private inFlight = new Set<string>();
  private cooldown = new Map<string, number>();
  private sendsThisTick = 0;
  private sendTimes: number[] = [];
  private consecutiveFailures = 0;
  private unknowns = 0;

  constructor(private readonly cfg: Config) {}

  startTick(): void {
    this.sendsThisTick = 0;
  }

  /** An operator's stop button that does not require finding the process. */
  halted(): boolean {
    return fs.existsSync(HALT);
  }

  /**
   * Refuse to send when the wallet is low.
   *
   * Prevents draining to the point where the keeper can no longer pay for the
   * transaction that would tell you it is stuck.
   */
  async solventEnough(connection: Connection, keeper: PublicKey): Promise<boolean> {
    const lamports = await connection.getBalance(keeper, "confirmed");
    return lamports >= this.cfg.minSolLamports;
  }

  /** Prevents a runaway loop burning the wallet between checks. */
  withinDailyBudget(nowUnix: number, aboutToSpend: number): boolean {
    const s = readState();
    const day = utcDay(nowUnix);
    const spent = s.day === day ? s.spentLamports : 0;
    return spent + aboutToSpend <= this.cfg.maxDailyLamports;
  }

  recordSpend(nowUnix: number, lamports: number): void {
    const day = utcDay(nowUnix);
    const s = readState();
    const spent = s.day === day ? s.spentLamports : 0;
    writeState({ day, spentLamports: spent + lamports });
  }

  /** Rate limits. Prevents one poisoned position monopolising the tick. */
  canSend(nowMs: number): boolean {
    if (this.sendsThisTick >= this.cfg.maxSendsPerTick) return false;
    this.sendTimes = this.sendTimes.filter((t) => nowMs - t < 3_600_000);
    return this.sendTimes.length < this.cfg.maxSendsPerHour;
  }

  noteSend(nowMs: number): void {
    this.sendsThisTick += 1;
    this.sendTimes.push(nowMs);
  }

  /**
   * The in-flight lock.
   *
   * Released only on ground truth — the position account being gone — or on a
   * demonstrably expired blockhash. NEVER on a timer: a timeout that unlocks a
   * position whose transaction actually landed is how a bot sends the same
   * liquidation twice and pays twice for one outcome.
   */
  claim(position: string): boolean {
    if (this.inFlight.has(position)) return false;
    this.inFlight.add(position);
    return true;
  }

  release(position: string): void {
    this.inFlight.delete(position);
  }

  isCoolingDown(position: string, nowMs: number): boolean {
    const until = this.cooldown.get(position);
    return until !== undefined && nowMs < until;
  }

  coolDown(position: string, nowMs: number): void {
    this.cooldown.set(position, nowMs + 30_000);
  }

  noteFailure(): void {
    this.consecutiveFailures += 1;
  }

  noteSuccess(): void {
    this.consecutiveFailures = 0;
  }

  breakerTripped(): boolean {
    return this.consecutiveFailures >= 5;
  }

  /**
   * Unknown verdicts are counted, not shrugged off.
   *
   * An unrecognised error means the bot's model of the program is wrong, and the
   * dangerous version of that is being wrong in the direction of "nothing to do".
   * Exiting loudly beats running blind; systemd restarts it, and a persistent
   * fault becomes a restart loop somebody notices.
   */
  noteUnknown(): boolean {
    this.unknowns += 1;
    return this.cfg.exitOnUnknown && this.unknowns >= this.cfg.unknownTolerance;
  }
}
