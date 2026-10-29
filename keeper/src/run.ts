/** Bounded fair scans, explicit degraded health, and journal-before-broadcast sends. */
import * as anchor from "@coral-xyz/anchor";
import path from "path";
import { loadConfig, loadKeypair, redactRpc } from "./config";
import { buildCtx, clusterNow, readExchange } from "./program";
import { buildCodeTable, isMarketWide } from "./errors";
import { loadMarkets, loadPositions, resolveOwnerTokenAccount, MarketRow } from "./discover";
import { needsPush, readPriceAge, OracleUpdater } from "./oracle";
import { buildLiquidateIx, simulate, prepareVerified, positionGone } from "./liquidate";
import { decide, profitWindowBps } from "./economics";
import { Guard } from "./guard";
import { log, heartbeat } from "./observe";
import { acquireLease, Journal } from "./journal";
import { broadcastReserved, reconcilePending } from "./pending";
import { Health, ScanCoverage, scanBatch, applyOperatingState } from "./scheduler";
import type { TransactionInstruction } from "@solana/web3.js";

const SUBSIDISED = process.env.SUBSIDISED === "1";
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, Math.max(0, ms)));

async function main(): Promise<void> {
  const cfg = loadConfig();
  const keeper = loadKeypair(cfg.keypairPath);
  const ctx = await buildCtx(cfg, keeper);
  const stateFile = path.resolve(__dirname, "..", ".state.json");
  const release = acquireLease(stateFile);
  process.once("exit", release);
  const identity = `${await ctx.connection.getGenesisHash()}:${ctx.program.programId}:${keeper.publicKey}`;
  const journal = new Journal(stateFile, identity);
  const guard = new Guard(cfg);
  const health = new Health();
  const coverage = new ScanCoverage();
  const table = buildCodeTable(ctx.idl);
  const updater = new OracleUpdater(ctx.connection, new anchor.Wallet(keeper));
  log.info("boot", {
    rpc: redactRpc(cfg.rpcUrl),
    keeper: keeper.publicKey.toBase58(),
    dryRun: cfg.dryRun,
    subsidised: SUBSIDISED,
  });

  for (;;) {
    const started = Date.now();
    guard.startTick();
    try {
      const now = await clusterNow(ctx.connection);
      // Every tick reconciles old signatures before even considering a new send.
      // An RPC exception leaves the durable pending record and its budget intact.
      for (const result of await reconcilePending(ctx.connection, journal, now.unix)) {
        if (
          result.kind === "finalized" &&
          !result.failed &&
          (await positionGone(ctx, new anchor.web3.PublicKey(result.pending.position)))
        ) {
          guard.noteSuccess();
          log.info("liquidated", {
            position: result.pending.position,
            signature: result.pending.signature,
            cost: result.cost,
          });
        } else {
          guard.noteFailure();
          guard.coolDown(result.pending.position, Date.now());
          log.warn("submission-failed", {
            position: result.pending.position,
            kind: result.kind,
            cost: result.cost,
          });
        }
      }
      health.recover("scan");
      if (journal.pending.length) health.fail("pending", "awaiting finalized transaction outcome");
      else health.recover("pending");

      // Market completeness/config is part of each successful book observation.
      const exchange = await readExchange(ctx);
      const markets = await loadMarkets(ctx);
      const positions = await loadPositions(ctx);
      if (markets.length !== exchange.numMarkets)
        throw new Error("market count does not match exchange");
      health.retain("position:", new Set(positions.map((p) => p.address.toBase58())));
      health.retain("market:", new Set(positions.map((p) => p.market.toBase58())));
      const marketByKey = new Map(markets.map((m) => [m.address.toBase58(), m]));
      const pushes = new Map<
        string,
        { market: MarketRow; instructions: TransactionInstruction[] } | null
      >();
      // Bound candidates, including failures and skips, not merely successful simulations.
      const candidates = scanBatch(
        positions,
        (p) => `${p.market}:${p.address}`,
        journal.cursor,
        cfg.maxSimsPerTick,
      );
      let simulated = 0;

      for (const position of candidates) {
        const pk = position.address.toBase58();
        const key = position.market.toBase58();
        const scope = `position:${pk}`;
        journal.advance(`${key}:${pk}`); // Survives restarts, removals and RPC order changes.
        if (journal.has(pk)) {
          health.fail(scope, "transaction pending");
          continue;
        }
        if (guard.isCoolingDown(pk, Date.now())) continue;
        try {
          const market = marketByKey.get(key);
          if (!market) {
            health.fail(scope, "unknown market");
            continue;
          }
          if (!pushes.has(key)) {
            try {
              const age = await readPriceAge(ctx.connection, market.priceUpdate, now);
              const needsUpdate = needsPush(age, {
                maxAgeSeconds: market.liquidationMaxAgeSeconds,
                maxAgeSlots: market.liquidationMaxAgeSlots,
              });
              const instructions = needsUpdate
                ? await updater.buildPushInstructions(
                    market.feedId.toString("hex"),
                    age?.view.publishTime ?? null,
                  )
                : null;
              // A newer update is required only outside the actual guards; the
              // earlier needsPush margin allows a healthy existing feed to work.
              if (
                (!age ||
                  age.seconds > market.liquidationMaxAgeSeconds ||
                  age.slots > market.liquidationMaxAgeSlots) &&
                !instructions?.length
              )
                throw new Error("stale oracle has no usable update");
              pushes.set(key, { market, instructions: instructions ?? [] });
            } catch (error) {
              pushes.set(key, null);
              health.fail(`market:${key}`, "oracle refresh unavailable");
              log.warn("market-oracle-unavailable", {
                market: key,
                detail: String(error).slice(0, 160),
              });
            }
          }
          const push = pushes.get(key);
          if (!push) continue;
          const ownerTokenAccount = await resolveOwnerTokenAccount(ctx, exchange, position.owner);
          if (!ownerTokenAccount) {
            health.fail(scope, "owner has no payout token account");
            continue;
          }
          const ix = await buildLiquidateIx(ctx, exchange, { position, market, ownerTokenAccount });
          const sim = await simulate(ctx, exchange, push.instructions, ix, table);
          simulated += 1;
          if (isMarketWide(sim.verdict)) {
            health.fail(`market:${key}`, sim.verdict.kind);
            pushes.set(key, null);
            log.warn("market-blocked", { market: key, verdict: sim.verdict });
            continue;
          }
          if (sim.verdict.kind === "healthy" || sim.verdict.kind === "liquidatable") {
            coverage.record(pk, Date.now());
            health.recover(`market:${key}`);
            health.recover(scope);
          }
          if (sim.verdict.kind === "healthy") continue;
          if (sim.verdict.kind !== "liquidatable") {
            health.fail(scope, sim.verdict.kind);
            log.error("candidate-unverified", { position: pk, verdict: sim.verdict });
            if (
              (sim.verdict.kind === "unknown" || sim.verdict.kind === "misconfig") &&
              guard.noteUnknown()
            )
              throw new Error("exiting loudly: repeated unrecognized simulations");
            continue;
          }
          const decision = decide(sim.fee, cfg, SUBSIDISED, sim.unitsConsumed);
          if (!decision.act) {
            health.fail(scope, decision.reason);
            continue;
          }
          if (profitWindowBps(market) <= 0) log.warn("market-cannot-pay-a-keeper", { market: key });
          if (cfg.dryRun) {
            health.fail(scope, "dry run: liquidation not submitted");
            continue;
          }
          if (guard.halted() || guard.breakerTripped() || journal.overrun) {
            health.fail(scope, "sending halted");
            continue;
          }
          if (
            !guard.canSend(Date.now()) ||
            journal.recentAttempts(now.unix) >= cfg.maxSendsPerHour
          ) {
            health.fail(scope, "send rate limited");
            continue;
          }
          const result = await prepareVerified(
            ctx,
            push.instructions,
            ix,
            sim.unitsConsumed,
            decision.priorityFeeMicroLamports,
            keeper,
          );
          if ("reason" in result) {
            health.fail(scope, result.reason);
            guard.noteFailure();
            continue;
          }
          const prepared = result.prepared;
          if (prepared.reservedLamports > journal.available(now.unix, cfg.maxDailyLamports)) {
            health.fail(scope, "daily budget exhausted");
            continue;
          }
          const balance = await ctx.connection.getBalance(keeper.publicKey, "confirmed");
          const outstanding = journal.pending.reduce((sum, p) => sum + p.reservedLamports, 0);
          if (balance - outstanding - prepared.reservedLamports < cfg.minSolLamports) {
            health.fail(scope, "below SOL floor after reservation");
            continue;
          }
          health.fail(scope, "transaction pending");
          await broadcastReserved(
            journal,
            {
              position: pk,
              signature: prepared.signature,
              blockhash: prepared.blockhash,
              lastValidBlockHeight: prepared.lastValidBlockHeight,
              reservedLamports: prepared.reservedLamports,
              submittedAt: now.unix,
            },
            cfg.maxDailyLamports,
            cfg.maxSendsPerHour,
            async () => {
              guard.noteSend(Date.now());
              return ctx.connection.sendRawTransaction(prepared.bytes, {
                skipPreflight: true,
                maxRetries: 3,
              });
            },
          );
          log.info("submitted", {
            position: pk,
            signature: prepared.signature,
            reservedLamports: prepared.reservedLamports,
          });
        } catch (error) {
          health.fail(
            scope,
            journal.has(pk)
              ? "ambiguous submission; awaiting reconciliation"
              : "candidate processing failed",
          );
          log.error("candidate-failed", { position: pk, detail: String(error).slice(0, 200) });
          if (String(error).includes("exiting loudly")) throw error;
        }
      }
      const unobserved = coverage.missing(
        new Set(positions.map((p) => p.address.toBase58())),
        Date.now(),
        cfg.maxScanAgeMs,
      );
      if (unobserved)
        health.fail("coverage", `${unobserved} positions have no recent verified simulation`);
      else health.recover("coverage");
      applyOperatingState(health, {
        halted: guard.halted(),
        breakerTripped: guard.breakerTripped(),
        budgetOverrun: journal.overrun,
        budgetExhausted: journal.available(now.unix, cfg.maxDailyLamports) === 0,
      });
      const healthy = health.healthy;
      log.info("tick", {
        positions: positions.length,
        simulated,
        pending: journal.pending.length,
        healthy,
        degraded: health.reasons,
      });
      await heartbeat(cfg.heartbeatUrl, healthy, Date.now());
    } catch (error) {
      health.fail("scan", "book or pending-transaction reconciliation failed");
      log.error("tick-failed", { detail: String(error).slice(0, 300) });
      if (String(error).includes("exiting loudly")) throw error;
    }
    await sleep(cfg.tickMs - (Date.now() - started));
  }
}

main().catch((error) => {
  log.error("fatal", { detail: String(error).slice(0, 400) });
  process.exit(1);
});
