/**
 * Prove the keeper works, without sending anything.
 *
 *   npx ts-node keeper/preflight.ts
 *
 * Env: RPC_URL, KEYPAIR.
 *
 * This exists because of a specific trap. Devnet currently has zero markets and
 * zero positions, so a correct keeper and a completely broken one produce the
 * same output: "nothing to do". Preflight therefore proves liveness POSITIVELY —
 * it decodes real accounts, measures the real oracle against the real guards, and
 * self-tests the error classifier — so that "0 positions" is a finding rather
 * than an absence of evidence.
 */
import { loadConfig, loadKeypair, redactRpc, POSITION_NOT_LIQUIDATABLE } from "./src/config";
import { buildCtx, clusterNow, readExchange } from "./src/program";
import { buildCodeTable, classify } from "./src/errors";
import { loadMarkets, loadPositions } from "./src/discover";
import { readPriceAge, needsPush } from "./src/oracle";
import { profitWindowBps } from "./src/economics";

const ok = (s: string) => `  PASS  ${s}`;
const bad = (s: string) => `  FAIL  ${s}`;
const note = (s: string) => `        ${s}`;

let failures = 0;
const check = (cond: boolean, msg: string) => {
  console.log(cond ? ok(msg) : bad(msg));
  if (!cond) failures += 1;
};

/**
 * The classifier is the one component whose failure is silent and total: if a
 * misconfiguration were read as `healthy`, the bot would report an empty book
 * forever while being entirely broken. This runs with no chain state at all, so
 * it is the one guarantee available on an empty devnet.
 */
function classifierSelfTest(table: ReturnType<typeof buildCodeTable>): void {
  console.log("\nclassifier self-test");

  const custom = (code: number) => ({ InstructionError: [1, { Custom: code }] });

  check(classify(null, [], table).kind === "liquidatable", "a clean simulation is liquidatable");

  check(
    classify(custom(POSITION_NOT_LIQUIDATABLE), [], table).kind === "healthy",
    "code 6062 alone is healthy",
  );

  // The inversion that would blind the bot.
  const misconfig = classify(custom(6068), [], table);
  check(
    misconfig.kind === "misconfig",
    `NotKeeperTokenOwner (6068) is misconfig, not healthy (got ${misconfig.kind})`,
  );

  const oracle = classify(custom(6003), [], table);
  check(
    oracle.kind === "market-oracle" || oracle.kind === "unknown",
    `an oracle code is market-wide or unknown, never healthy (got ${oracle.kind})`,
  );

  check(
    classify({ InstructionError: [1, "AccountNotInitialized"] }, [], table).kind ===
      "owner-account-missing",
    "a missing token account is its own verdict",
  );

  check(
    classify(custom(999_999), [], table).kind === "unknown",
    "an unmapped code is unknown, never healthy",
  );

  // A code that says 6062 while the log names something else must not be trusted.
  const contradictory = classify(
    custom(POSITION_NOT_LIQUIDATABLE),
    ["Program log: AnchorError ... Error Message: Oracle price is stale."],
    table,
  );
  check(
    contradictory.kind === "unknown",
    `a 6062 code contradicted by its log is unknown (got ${contradictory.kind})`,
  );
}

async function main(): Promise<void> {
  const cfg = loadConfig();
  const keeper = loadKeypair(cfg.keypairPath);

  console.log(`rpc     ${redactRpc(cfg.rpcUrl)}`);
  console.log(`keeper  ${keeper.publicKey.toBase58()}`);

  const ctx = await buildCtx(cfg, keeper);
  console.log(ok("genesis hash is devnet (buildCtx refuses anything else)"));

  const table = buildCodeTable(ctx.idl);
  console.log(
    ok(`IDL error table loaded; ${POSITION_NOT_LIQUIDATABLE} is PositionNotLiquidatable`),
  );

  console.log("\nexchange");
  const ex = await readExchange(ctx);
  console.log(note(`collateral mint    ${ex.collateralMint.toBase58()}`));
  console.log(note(`token program      ${ex.collateralTokenProgram.toBase58()}`));
  console.log(note(`decimals           ${ex.collateralDecimals}`));
  console.log(note(`paused flags       ${ex.pausedFlags}`));
  console.log(note(`keeper fee share   ${ex.keeperFeeShareBps} bps`));
  console.log(note(`markets            ${ex.numMarkets}`));
  check(
    ex.collateralDecimals > 0,
    "exchange decoded (a zero here means the Anchor path is broken)",
  );
  check(
    ex.keeperFeeShareBps > 0,
    `keeper share is non-zero (${ex.keeperFeeShareBps} bps) — at 0 a keeper is never paid`,
  );

  const now = await clusterNow(ctx.connection);
  console.log(note(`cluster time       ${now.unix}  slot ${now.slot}`));

  console.log("\nbook");
  const markets = await loadMarkets(ctx);
  const positions = await loadPositions(ctx);
  console.log(note(`markets found      ${markets.length}`));
  console.log(note(`positions found    ${positions.length}`));
  check(
    markets.length === ex.numMarkets,
    `market count matches exchange.numMarkets (${markets.length} vs ${ex.numMarkets})`,
  );

  if (markets.length === 0) {
    console.log(
      note(
        "no markets exist yet — the keeper will correctly do nothing until\n" +
          "        qualify_feed -> create_market -> set_risk_params -> open_position has run.",
      ),
    );
  }

  for (const m of markets) {
    console.log(`\nmarket ${m.address.toBase58()}`);
    const window = profitWindowBps(m);
    console.log(note(`profit window      ${window} bps`));
    check(window > 0, "the market can pay a keeper at all");

    const age = await readPriceAge(ctx.connection, m.priceUpdate, now);
    if (!age) {
      check(false, "price account exists");
      continue;
    }
    console.log(note(`price              ${age.view.price} x 10^${age.view.exponent}`));
    console.log(
      note(
        `age                ${age.seconds}s / ${age.slots} slots  ` +
          `(guards ${m.liquidationMaxAgeSeconds}s / ${m.liquidationMaxAgeSlots} slots)`,
      ),
    );
    const stale = age.seconds > m.liquidationMaxAgeSeconds || age.slots > m.liquidationMaxAgeSlots;
    console.log(
      note(
        stale
          ? "feed is OUTSIDE the liquidation guards right now — the keeper must carry its own push"
          : "feed is currently inside the liquidation guards",
      ),
    );
    console.log(
      note(
        `push needed        ${needsPush(age, {
          maxAgeSeconds: m.liquidationMaxAgeSeconds,
          maxAgeSlots: m.liquidationMaxAgeSlots,
        })}`,
      ),
    );
  }

  classifierSelfTest(table);

  console.log("");
  if (failures > 0) {
    console.log(`${failures} CHECK(S) FAILED — do not run the keeper until these pass`);
    process.exit(1);
  }
  console.log("all preflight checks passed");
  if (positions.length === 0) {
    console.log("note: zero positions. The keeper is live and correct, and has nothing to do.");
  }
}

main().catch((e) => {
  console.error("preflight failed:", e);
  process.exit(1);
});
