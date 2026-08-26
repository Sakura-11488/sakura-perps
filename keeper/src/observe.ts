/**
 * Structured output, and the heartbeat that distinguishes "quiet" from "dead".
 *
 * The modal failure of an unattended keeper is not a crash — it is running
 * happily while doing nothing, which looks exactly like a healthy market. The
 * heartbeat only fires while the bot is genuinely working, so silence means
 * something is wrong rather than something is calm.
 */
import fs from "fs";
import path from "path";
import { redactRpc } from "./config";

const logDir = path.resolve(__dirname, "..", "logs");

type Fields = Record<string, unknown>;

let auditStream: fs.WriteStream | null = null;

function audit(): fs.WriteStream {
  if (!auditStream) {
    fs.mkdirSync(logDir, { recursive: true });
    const day = new Date().toISOString().slice(0, 10);
    auditStream = fs.createWriteStream(path.join(logDir, `keeper-${day}.jsonl`), { flags: "a" });
  }
  return auditStream;
}

/**
 * Every emitted string passes through here.
 *
 * The RPC URLs this project uses carry an API key in the query string. Logs get
 * pasted into issues and shipped to log aggregators; one un-redacted line is a
 * leaked credential that nobody notices leaking.
 */
function scrub(value: unknown): unknown {
  if (typeof value === "string") {
    if (value.includes("api-key") || value.includes("://")) {
      return value.replace(/https?:\/\/[^\s"']+/g, (m) => redactRpc(m));
    }
    return value;
  }
  if (Array.isArray(value)) return value.map(scrub);
  if (value && typeof value === "object") {
    const out: Fields = {};
    for (const [k, v] of Object.entries(value as Fields)) out[k] = scrub(v);
    return out;
  }
  return value;
}

function emit(level: "info" | "warn" | "error", event: string, fields: Fields): void {
  const line = JSON.stringify(scrub({ ts: new Date().toISOString(), level, event, ...fields }));
  if (level === "error") console.error(line);
  else console.log(line);
  audit().write(`${line}\n`);
}

export const log = {
  info: (event: string, fields: Fields = {}) => emit("info", event, fields),
  warn: (event: string, fields: Fields = {}) => emit("warn", event, fields),
  error: (event: string, fields: Fields = {}) => emit("error", event, fields),
};

let lastHeartbeat = 0;

/**
 * Ping only while healthy.
 *
 * Deliberately NOT sent when halted or stalled — a heartbeat that fires
 * regardless of state tells you the process exists, which was never the
 * question.
 */
export async function heartbeat(
  url: string | undefined,
  healthy: boolean,
  nowMs: number,
): Promise<void> {
  if (!url || !healthy) return;
  if (nowMs - lastHeartbeat < 60_000) return;
  lastHeartbeat = nowMs;
  try {
    await fetch(url, { method: "GET" });
  } catch (e) {
    log.warn("heartbeat-failed", { detail: String(e).slice(0, 120) });
  }
}
