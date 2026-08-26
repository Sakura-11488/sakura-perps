/**
 * The tick loop. The only file here with control flow.
 *
 *   npx ts-node keeper/src/run.ts
 *
 * Env: RPC_URL, KEYPAIR. Everything else has a default in config.ts.
 * DRY_RUN defaults ON — set DRY_RUN=0 to actually send.
 */
import * as anchor from "@coral-xyz/anchor";
import { loadConfig, loadKeypair, redactRpc } from "./config";
import { buildCtx, clusterNow, readExchange, ExchangeView } from "./program";
import { buildCodeTable, CodeTable, isMarketWide } from "./errors";
import {
  loadMarkets,
  loadPositions,
  groupByMarket,
  resolveOwnerTokenAccount,
  MarketRow,
} from "./discover";
import { needsPush, readPriceAge, OracleUpdater } from "./oracle";
import { buildLiquidateIx, simulate, sendVerified, positionGone } from "./liquidate";
import { decide, profitWindowBps } from "./economics";
import { Guard } from "./guard";
import { log, heartbeat } from "./observe";

const SUBSIDISED = process.env.SUBSIDISED === "1";

async function main(): Promise<void> {
  const cfg = loadConfig();
  const keeper = loadKeypair(cfg.keypairPath);
  const ctx = await buildCtx(cfg, keeper);
  const table: CodeTable = buildCodeTable(ctx.idl);
  const guard = new Guard(cfg);
  const updater = new OracleUpdater(ctx.connection, new anchor.Wallet(keeper));

  log.info("boot", {
    rpc: redactRpc(cfg.rpcUrl),
    keeper: keeper.publicKey.toBase58(),
    dryRun: cfg.dryRun,
    subsidised: SUBSIDISED,
    tickMs: cfg.tickMs,
  });

  let exchange: ExchangeView = await readExchange(ctx);
  let markets: MarketRow[] = await loadMarkets(ctx);
  let ticks = 0;

  for (;;) {
    const started = Date.now();
    guard.startTick();
    try {
      if (ticks % cfg.marketRefreshTicks === 0) {
        exchange = await readExchange(ctx);
        markets = await loadMarkets(ctx);
        if (markets.length !== exchange.numMarkets) {
          log.warn("market-count-mismatch", {
            found: markets.length,
            expected: exchange.numMarkets,
          });
        }
        for (const m of markets) {
          const window = profitWindowBps(m);
          if (window <= 0) {
            log.warn("market-cannot-pay-a-keeper", {
              market: m.address.toBase58(),
              windowBps: window,
            });
          }
        }
      }
      ticks += 1;

      const now = await clusterNow(ctx.connection);
      const positions = await loadPositions(ctx);

      if (positions.length === 0) {
        log.info("tick", { positions: 0, markets: markets.length, note: "nothing to do" });
        await heartbeat(cfg.heartbeatUrl, !guard.halted(), Date.now());
        await sleep(cfg.tickMs - (Date.now() - started));
        continue;
      }

      const byMarket = groupByMarket(positions);
      const marketByKey = new Map(markets.map((m) => [m.address.toBase58(), m]));
      let simulated = 0;

      for (const [key, rows] of byMarket) {
        const market = marketByKey.get(key);
        if (!market) {
          log.warn("unknown-market", { market: key, positions: rows.length });
          continue;
        }

        // One oracle read and at most one Hermes fetch per market per tick.
        const age = await readPriceAge(ctx.connection, market.priceUpdate, now);
        const push = needsPush(age, {
          maxAgeSeconds: market.liquidationMaxAgeSeconds,
          maxAgeSlots: market.liquidationMaxAgeSlots,
        })
          ? await updater
              .buildPushInstructions(market.feedId.toString("hex"), age?.view.publishTime ?? null)
              .catch((e) => {
                log.warn("push-build-failed", { market: key, detail: String(e).slice(0, 160) });
                return null;
              })
          : null;

        for (const position of rows) {
          if (simulated >= cfg.maxSimsPerTick) break;
          const pk = position.address.toBase58();
          if (guard.isCoolingDown(pk, Date.now())) continue;
          if (!guard.claim(pk)) continue;

          try {
            const ownerTokenAccount = await resolveOwnerTokenAccount(ctx, exchange, position.owner);
            if (!ownerTokenAccount) {
              log.info("skip", { position: pk, reason: "owner-has-no-token-account" });
              continue;
            }

            const ix = await buildLiquidateIx(ctx, exchange, {
              position,
              market,
              ownerTokenAccount,
            });
            const sim = await simulate(ctx, exchange, push ?? [], ix, table);
            simulated += 1;

            if (sim.verdict.kind === "healthy") continue;

            if (isMarketWide(sim.verdict)) {
              log.warn("market-blocked", { market: key, verdict: sim.verdict });
              break; // the whole market is unusable this tick
            }

            if (sim.verdict.kind === "misconfig") {
              log.error("misconfig", { position: pk, verdict: sim.verdict });
              if (guard.noteUnknown())
                throw new Error("too many misconfigurations; exiting loudly");
              continue;
            }
            if (sim.verdict.kind === "unknown") {
              log.error("unknown-verdict", { position: pk, detail: sim.verdict.detail });
              if (guard.noteUnknown()) throw new Error("too many unknown verdicts; exiting loudly");
              continue;
            }
            if (sim.verdict.kind === "owner-account-missing") continue;

            // liquidatable
            const decision = decide(sim.fee, cfg, SUBSIDISED, sim.unitsConsumed);
            if (!decision.act) {
              log.info("skip", {
                position: pk,
                reason: decision.reason,
                feeQuote: sim.fee.keeperFeeQuote.toString(),
              });
              continue;
            }

            log.info("liquidatable", {
              position: pk,
              feeQuote: sim.fee.keeperFeeQuote.toString(),
              units: sim.unitsConsumed,
              pushed: (push ?? []).length > 0,
            });

            if (cfg.dryRun) {
              log.info("dry-run", { position: pk, note: "would send; set DRY_RUN=0 to act" });
              continue;
            }
            if (guard.halted() || guard.breakerTripped()) {
              log.warn("send-suppressed", { position: pk, halted: guard.halted() });
              continue;
            }
            if (!guard.canSend(Date.now())) continue;
            if (!(await guard.solventEnough(ctx.connection, keeper.publicKey))) {
              log.error("below-sol-floor", { keeper: keeper.publicKey.toBase58() });
              continue;
            }
            if (!guard.withinDailyBudget(now.unix, 10_000)) {
              log.warn("daily-budget-exhausted", {});
              continue;
            }

            const out = await sendVerified(
              ctx,
              push ?? [],
              ix,
              sim.unitsConsumed,
              decision.priorityFeeMicroLamports,
              keeper,
            );
            guard.noteSend(Date.now());
            guard.recordSpend(now.unix, 10_000);

            if (!out.sent) {
              guard.noteFailure();
              guard.coolDown(pk, Date.now());
              log.warn("send-refused", { position: pk, reason: out.reason });
              continue;
            }

            // A confirmed signature is not success. Losing the race also confirms.
            const gone = await positionGone(ctx, position.address);
            if (gone) {
              guard.noteSuccess();
              log.info("liquidated", { position: pk, signature: out.signature });
            } else {
              guard.noteFailure();
              guard.coolDown(pk, Date.now());
              log.warn("sent-but-position-remains", { position: pk, signature: out.signature });
            }
          } finally {
            guard.release(pk);
          }
        }
      }

      log.info("tick", { positions: positions.length, simulated, markets: markets.length });
      await heartbeat(cfg.heartbeatUrl, !guard.halted() && !guard.breakerTripped(), Date.now());
    } catch (e) {
      log.error("tick-failed", { detail: String(e).slice(0, 300) });
      if (String(e).includes("exiting loudly")) process.exit(1);
    }
    await sleep(cfg.tickMs - (Date.now() - started));
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, Math.max(0, ms)));

main().catch((e) => {
  log.error("fatal", { detail: String(e).slice(0, 400) });
  process.exit(1);
});
