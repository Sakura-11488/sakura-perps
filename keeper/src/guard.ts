/** In-process cooldown/breaker; pending transactions and spending live in Journal. */
import fs from "fs";
import path from "path";
import type { Config } from "./config";

const HALT = path.resolve(__dirname, "..", "HALT");

export class Guard {
  private cooldown = new Map<string, number>();
  private sendsThisTick = 0;
  private consecutiveFailures = 0;
  private unknowns = 0;

  constructor(private readonly cfg: Config) {}
  startTick(): void {
    this.sendsThisTick = 0;
  }
  halted(): boolean {
    return fs.existsSync(HALT);
  }
  canSend(_nowMs: number): boolean {
    return this.sendsThisTick < this.cfg.maxSendsPerTick;
  }
  noteSend(_nowMs: number): void {
    this.sendsThisTick += 1;
  }
  isCoolingDown(position: string, nowMs: number): boolean {
    return (this.cooldown.get(position) ?? 0) > nowMs;
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
  noteUnknown(): boolean {
    this.unknowns += 1;
    return this.cfg.exitOnUnknown && this.unknowns >= this.cfg.unknownTolerance;
  }
}
