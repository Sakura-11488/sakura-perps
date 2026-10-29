import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import {
  Keypair,
  SystemProgram,
  VersionedTransaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { Health, ScanCoverage, scanBatch, applyOperatingState } from "../src/scheduler";
import { acquireLease, Journal, PendingTransaction } from "../src/journal";
import { broadcastReserved, reconcilePending, reserveCost } from "../src/pending";
import { loadConfig, PROGRAM_ID } from "../src/config";
import { prepareVerified, simulate } from "../src/liquidate";
import type { Ctx } from "../src/program";
import type { ExchangeView } from "../src/program";

const NOW = 1_800_000_000;
function fixture(t: { after: (fn: () => void) => void }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sakura-keeper-test-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, ".state.json");
  return { file, journal: new Journal(file, "devnet:program:keeper") };
}
function pending(position = "p1", cost = 30_000): Omit<PendingTransaction, "day"> {
  return {
    position,
    signature: `sig-${position}`,
    blockhash: "hash",
    lastValidBlockHeight: 123,
    reservedLamports: cost,
    submittedAt: NOW,
  };
}
function rpc(overrides: Record<string, unknown> = {}) {
  return {
    getSignatureStatuses: async () => ({ value: [null] }),
    getBlockHeight: async () => 100,
    getTransaction: async () => null,
    ...overrides,
  } as unknown as Parameters<typeof reconcilePending>[0];
}

test("stable healthy prefixes and multiple markets cannot starve a >cap book across restarts", (t) => {
  const { file, journal } = fixture(t);
  const rows = Array.from({ length: 103 }, (_, i) => ({
    key: `${i < 70 ? "a" : "z"}:${String(i).padStart(3, "0")}`,
    healthy: i !== 102,
  }));
  const visited = new Set<string>();
  let state = journal;
  for (let tick = 0; tick < 3; tick++) {
    const batch = scanBatch([...rows].reverse(), (row) => row.key, state.cursor, 40);
    assert.equal(batch.length, 40);
    for (const row of batch) {
      visited.add(row.key);
      state.advance(row.key);
    }
    state = new Journal(file, "devnet:program:keeper");
  }
  assert.equal(visited.size, rows.length);
  assert.ok(visited.has("z:102"));
  assert.deepEqual(
    scanBatch(["a", "c", "d"], (x) => x, "b", 2),
    ["c", "d"],
  );
  assert.deepEqual(
    scanBatch(["a", "c"], (x) => x, "z", 2),
    ["a", "c"],
  );
});

test("oracle and scan failures remain degraded until the affected scope recovers", () => {
  const health = new Health();
  health.fail("market:a", "oracle unavailable");
  health.recover("market:b");
  assert.equal(health.healthy, false);
  health.fail("scan", "RPC unavailable");
  health.recover("market:a");
  assert.equal(health.healthy, false);
  health.recover("scan");
  assert.equal(health.healthy, true);
  health.fail("position:closed", "pending");
  health.retain("position:", new Set(["open"]));
  assert.equal(health.healthy, true);
});

test("unknown and stale scan coverage cannot claim a healthy book", () => {
  const coverage = new ScanCoverage();
  const positions = new Set(Array.from({ length: 41 }, (_, i) => String(i)));
  for (let i = 0; i < 40; i++) coverage.record(String(i), 100);
  assert.equal(coverage.missing(positions, 100, 60_000), 1);
  coverage.record("40", 200);
  assert.equal(coverage.missing(positions, 200, 60_000), 0);
  assert.equal(coverage.missing(positions, 60_101, 60_000), 40);
  assert.equal(coverage.missing(positions, 50, 60_000), 41, "clock regression fails closed");
  assert.equal(coverage.missing(new Set(), 60_101, 60_000), 0);
});

test("an empty healthy book still reports HALT, breaker and budget stops in the shared health verdict", () => {
  const health = new Health();
  const running = {
    halted: false,
    breakerTripped: false,
    budgetOverrun: false,
    budgetExhausted: false,
  };
  for (const reason of Object.keys(running)) {
    applyOperatingState(health, { ...running, [reason]: true });
    assert.equal(health.healthy, false);
    assert.equal(health.reasons[`operator:${reason}`], reason);
    applyOperatingState(health, running);
    assert.equal(health.healthy, true, "cleared operator state must not remain stale");
  }
});

test("broadcast ambiguity stays locked and reserved after process restart", async (t) => {
  const { file, journal } = fixture(t);
  let sent = 0;
  await assert.rejects(
    broadcastReserved(journal, pending(), 100_000, 10, async () => {
      // Inspect disk from a new process perspective before the mock RPC receives bytes.
      const saved = new Journal(file, "devnet:program:keeper");
      assert.ok(saved.has("p1"));
      assert.equal(saved.available(NOW, 100_000), 70_000);
      sent++;
      throw new Error("RPC lost its response after accepting the transaction");
    }),
    /RPC lost/,
  );
  const restarted = new Journal(file, "devnet:program:keeper");
  assert.equal(restarted.has("p1"), true);
  await assert.rejects(
    broadcastReserved(restarted, pending(), 100_000, 10, async () => {
      sent++;
      return "sig-p1";
    }),
    /already pending/,
  );
  await assert.rejects(
    reconcilePending(
      rpc({
        getSignatureStatuses: async () => {
          throw new Error("offline");
        },
      }),
      restarted,
      NOW + 1,
    ),
    /offline/,
  );
  assert.equal(restarted.pending.length, 1);
  assert.equal(sent, 1);
});

test("finalized receipts reconcile network, priority and oracle spend, including failed transactions", async (t) => {
  const { journal } = fixture(t);
  journal.reserve(pending("p1", 3_000_000), 10_000_000, 10);
  const result = await reconcilePending(
    rpc({
      getSignatureStatuses: async () => ({ value: [{ confirmationStatus: "finalized" }] }),
      getTransaction: async () => ({
        meta: { fee: 205_000, preBalances: [9_000_000], postBalances: [6_760_000], err: null },
      }),
    }),
    journal,
    NOW + 30,
  );
  assert.equal(result[0].cost, 2_240_000);
  assert.equal(journal.available(NOW + 30, 10_000_000), 7_760_000);
  assert.equal(journal.pending.length, 0);
  journal.reserve(pending("p2", 1_000_000), 10_000_000, 10);
  await reconcilePending(
    rpc({
      getSignatureStatuses: async () => ({ value: [{ confirmationStatus: "finalized" }] }),
      getTransaction: async () => ({
        meta: {
          fee: 205_000,
          preBalances: [6_760_000],
          postBalances: [6_555_000],
          err: { InstructionError: [2, "failed"] },
        },
      }),
    }),
    journal,
    NOW + 60,
  );
  assert.equal(journal.available(NOW + 60, 10_000_000), 7_555_000);
});

test("confirmation and absent metadata cannot release; expiry requires finalized height and a second status read", async (t) => {
  const { journal } = fixture(t);
  journal.reserve(pending(), 100_000, 10);
  for (const confirmationStatus of ["processed", "confirmed", "finalized"]) {
    await reconcilePending(
      rpc({
        getSignatureStatuses: async () => ({ value: [{ confirmationStatus }] }),
        getBlockHeight: async () => 999,
      }),
      journal,
      NOW,
    );
    assert.equal(journal.pending.length, 1);
  }
  let reads = 0;
  await reconcilePending(
    rpc({
      getSignatureStatuses: async () => ({
        value: [++reads === 1 ? null : { confirmationStatus: "confirmed" }],
      }),
      getBlockHeight: async () => 124,
    }),
    journal,
    NOW,
  );
  assert.equal(journal.pending.length, 1);
  const expired = await reconcilePending(rpc({ getBlockHeight: async () => 124 }), journal, NOW);
  assert.equal(expired[0].kind, "expired");
  assert.equal(journal.pending.length, 0);
  assert.equal(
    journal.available(NOW, 100_000),
    70_000,
    "absence is not a refund of unprovable spend",
  );
});

test("reservations and hourly attempts survive restart and midnight; underestimated spend halts sends", async (t) => {
  const { file, journal } = fixture(t);
  journal.reserve(pending("p1", 60_000), 100_000, 1);
  const next = new Journal(file, "devnet:program:keeper");
  assert.equal(next.available(NOW + 86_400, 100_000), 40_000);
  assert.throws(() => next.reserve(pending("p2", 50_000), 100_000, 10), /budget exhausted/);
  assert.throws(() => next.reserve(pending("p2", 1), 100_000, 1), /hourly/);
  next.settle("sig-p1", 65_000, NOW);
  assert.equal(next.overrun, true);
  assert.throws(() => next.reserve(pending("p2", 1), 100_000, 10), /budget exhausted/);
  assert.equal(new Journal(file, "devnet:program:keeper").overrun, true);
});

test("journal corruption, identity mismatch and unsafe integer amounts fail closed", (t) => {
  const { file } = fixture(t);
  assert.throws(() => new Journal(file, "different wallet"), /another wallet/);
  fs.writeFileSync(file, "{");
  assert.throws(() => new Journal(file, "devnet:program:keeper"), /unreadable/);
  assert.equal(fs.readFileSync(file, "utf8"), "{");
  fs.writeFileSync(file, JSON.stringify({ day: "2027-01-15", spentLamports: 42 }));
  new Journal(file, "devnet:program:keeper");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).spent["2027-01-15"], 42);
  assert.throws(() => reserveCost(Number.MAX_SAFE_INTEGER, 0, 5000), /lamport/);
});

test("array ledgers and malformed dates are rejected without rewriting their file", (t) => {
  const { file } = fixture(t);
  const valid = JSON.parse(fs.readFileSync(file, "utf8"));
  for (const spent of [[], { "not-a-day": 10 }, { "2026-02-30": 10 }, { "2026-10-29": -1 }]) {
    const malformed = JSON.stringify({ ...valid, spent });
    fs.writeFileSync(file, malformed);
    assert.throws(() => new Journal(file, "devnet:program:keeper"), /journal invalid/);
    assert.equal(fs.readFileSync(file, "utf8"), malformed);
  }
  for (const day of ["not-a-day", "2026-02-30"]) {
    const malformed = JSON.stringify({ day, spentLamports: 10 });
    fs.writeFileSync(file, malformed);
    assert.throws(() => new Journal(file, "devnet:program:keeper"), /journal invalid/);
    assert.equal(fs.readFileSync(file, "utf8"), malformed);
  }
});

test("process lease refuses a second writer", (t) => {
  const { file } = fixture(t);
  const release = acquireLease(file);
  assert.throws(() => acquireLease(file), /another keeper/);
  release();
  acquireLease(file)();
});

test("a dead process lease can restart without discarding its pending transactions", (t) => {
  const { file, journal } = fixture(t);
  journal.reserve(pending(), 100_000, 10);
  const child = spawnSync(process.execPath, ["-e", "process.stdout.write(String(process.pid))"], {
    encoding: "utf8",
  });
  assert.equal(child.status, 0);
  fs.writeFileSync(`${file}.lock`, child.stdout);
  const release = acquireLease(file);
  assert.equal(new Journal(file, "devnet:program:keeper").has("p1"), true);
  release();
});

test("a failed durable reservation never broadcasts and latches further spending off", async (t) => {
  const { file, journal } = fixture(t);
  fs.mkdirSync(`${file}.tmp`); // Forces the durable-write step to fail.
  let sent = false;
  await assert.rejects(
    broadcastReserved(journal, pending(), 100_000, 10, async () => {
      sent = true;
      return "sig-p1";
    }),
  );
  assert.equal(sent, false);
  assert.equal(journal.overrun, true);
  assert.throws(() => journal.reserve(pending("p2"), 100_000, 10), /budget/);
});

test("exact-message preparation measures oracle debit and network fee without broadcasting", async () => {
  const keeper = Keypair.generate();
  const blockhash = Keypair.generate().publicKey.toBase58();
  let simulated: VersionedTransaction | null = null;
  const connection = {
    getLatestBlockhash: async () => ({ blockhash, lastValidBlockHeight: 900 }),
    getBalanceAndContext: async () => ({ context: { slot: 50 }, value: 10_000_000 }),
    getFeeForMessage: async () => ({ value: 25_000 }),
    simulateTransaction: async (tx: VersionedTransaction, options: { minContextSlot: number }) => {
      simulated = tx;
      assert.equal(options.minContextSlot, 50);
      return { context: { slot: 50 }, value: { err: null, accounts: [{ lamports: 7_900_000 }] } };
    },
  };
  const ctx = { connection } as unknown as Ctx;
  const ix = SystemProgram.transfer({
    fromPubkey: keeper.publicKey,
    toPubkey: Keypair.generate().publicKey,
    lamports: 2_100_000,
  });
  const result = await prepareVerified(ctx, [ix], ix, 80_000, 1_000, keeper);
  assert.ok("prepared" in result);
  assert.equal(result.prepared.reservedLamports, reserveCost(10_000_000, 7_900_000, 25_000));
  assert.deepEqual(
    VersionedTransaction.deserialize(result.prepared.bytes).message.serialize(),
    (simulated! as VersionedTransaction).message.serialize(),
  );
  assert.equal(result.prepared.lastValidBlockHeight, 900);
});

test("malformed safety flags and limits cannot silently disable safeguards", () => {
  const names = [
    "RPC_URL",
    "KEYPAIR",
    "DRY_RUN",
    "EXIT_ON_UNKNOWN",
    "ALLOW_ANY_CLUSTER",
    "TICK_MS",
    "MAX_SIMS_PER_TICK",
    "MAX_DAILY_LAMPORTS",
    "PRIORITY_FEE_SHARE_BPS",
  ];
  const saved = Object.fromEntries(names.map((name) => [name, process.env[name]]));
  try {
    names.forEach((name) => delete process.env[name]);
    process.env.RPC_URL = "http://localhost";
    process.env.KEYPAIR = "/test/keeper.json";
    assert.equal(loadConfig().dryRun, true);
    for (const [name, values] of Object.entries({
      DRY_RUN: ["no", "tru", " "],
      EXIT_ON_UNKNOWN: ["off"],
      ALLOW_ANY_CLUSTER: ["yes"],
      TICK_MS: ["0", "-1", "1.5", "Infinity"],
      MAX_SIMS_PER_TICK: ["0"],
      MAX_DAILY_LAMPORTS: ["9007199254740992", "-1"],
      PRIORITY_FEE_SHARE_BPS: ["10001"],
    })) {
      for (const value of values) {
        process.env[name] = value;
        assert.throws(() => loadConfig(), new RegExp(name));
      }
      delete process.env[name];
    }
  } finally {
    for (const [name, value] of Object.entries(saved)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
});

test("intervening payer credits cannot mask cost through unmatched RPC snapshots", async () => {
  const keeper = Keypair.generate();
  let balanceReads = 0;
  const ctx = {
    connection: {
      getLatestBlockhash: async () => ({
        blockhash: Keypair.generate().publicKey.toBase58(),
        lastValidBlockHeight: 900,
      }),
      getBalanceAndContext: async () =>
        ++balanceReads === 1
          ? { context: { slot: 50 }, value: 10_000_000 }
          : { context: { slot: 52 }, value: 30_000_000 },
      getFeeForMessage: async () => ({ value: 25_000 }),
      simulateTransaction: async () => ({
        context: { slot: 51 },
        value: { err: null, accounts: [{ lamports: 29_000_000 }] },
      }),
    },
  } as unknown as Ctx;
  const ix = SystemProgram.transfer({
    fromPubkey: keeper.publicKey,
    toPubkey: Keypair.generate().publicKey,
    lamports: 1_000_000,
  });
  const result = await prepareVerified(ctx, [], ix, 80_000, 1_000, keeper);
  assert.deepEqual(result, { reason: "payer-snapshot-changed" });
  assert.equal(balanceReads, 2);
});

test("push and CPI custom-code collisions never classify a position as healthy", async () => {
  const keeper = Keypair.generate();
  const ix = new TransactionInstruction({ programId: PROGRAM_ID, keys: [], data: Buffer.alloc(0) });
  const push = SystemProgram.transfer({
    fromPubkey: keeper.publicKey,
    toPubkey: Keypair.generate().publicKey,
    lamports: 1,
  });
  const exchange = {
    collateralMint: Keypair.generate().publicKey,
    collateralTokenProgram: Keypair.generate().publicKey,
  } as ExchangeView;
  const table = { byCode: new Map([[6062, "PositionNotLiquidatable"]]) };
  const ownFailure = `Program ${PROGRAM_ID} failed: custom program error: 0x17ae`;
  for (const [index, logs, kind] of [
    [1, [ownFailure], "unknown"], // Pyth push, even with a spoofed matching error number.
    [2.5, [ownFailure], "unknown"],
    [-1, [ownFailure], "unknown"],
    [
      2,
      [`Program ${SystemProgram.programId} failed: custom program error: 0x17ae`, ownFailure],
      "unknown",
    ],
    [2, [], "unknown"],
    [2, [ownFailure], "healthy"],
  ] as Array<[number, string[], string]>) {
    const ctx = {
      keeper,
      connection: {
        getAccountInfo: async () => null,
        simulateTransaction: async () => ({
          value: {
            err: { InstructionError: [index, { Custom: 6062 }] },
            logs,
            unitsConsumed: 10_000,
          },
        }),
      },
    } as unknown as Ctx;
    const result = await simulate(ctx, exchange, [push], ix, table);
    assert.equal(result.verdict.kind, kind, `failing instruction ${index}: ${logs}`);
  }
});
