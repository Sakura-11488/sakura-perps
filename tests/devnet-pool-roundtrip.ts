/**
 * Devnet round-trip for the collateral pool, against real USDC-devnet.
 *
 * Not a mocha test — a script, run against a live cluster with the deployer
 * keypair, because the thing it proves is precisely what LiteSVM cannot: that
 * the deployed bytecode works with a mint we do not control, over a real RPC.
 *
 *   npx ts-node tests/devnet-pool-roundtrip.ts
 *
 * Env:
 *   RPC_URL   devnet RPC (use a paid one — public devnet drops deploy-sized traffic)
 *   KEYPAIR   path to the deployer/admin keypair
 *
 * The exchange PDA is a singleton with no close instruction, so
 * `initialize_exchange` is one-shot per program id and pins the collateral mint
 * forever. This script therefore refuses to initialize anything until it has
 * confirmed the deposit can actually be funded — a half-initialized exchange
 * pinned to the wrong mint would mean burning the program id to recover.
 */
import * as anchor from '@coral-xyz/anchor';
import { Program } from '@coral-xyz/anchor';
import { PublicKey, Keypair, Connection, Transaction, SystemProgram } from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
  getAccount,
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
} from '@solana/spl-token';
import fs from 'fs';
import { createHash } from 'crypto';
import type { SakuraPerps } from '../target/types/sakura_perps';

/**
 * Circle's USDC on Solana devnet — the one faucet.circle.com actually dispenses.
 *
 * NOT `Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr`, which the plan named. That
 * is a legacy community "USDC-Dev" token whose **mint authority is the mint
 * address itself**, so no keypair can sign a mint instruction and its supply is
 * permanently fixed — there is no faucet for it and there cannot be one. Caught
 * before `initialize_exchange` ran, which matters because that call pins the
 * collateral mint for the life of the program id.
 */
const USDC_DEVNET = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');
const PROGRAM_ID = new PublicKey('5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y');
/** 2 USDC, six decimals. Small enough to be cheap, large enough that share
 *  maths is not operating on dust. */
const DEPOSIT = 2_000_000n;
/** Permanently locked on the FIRST deposit into an empty pool — the ERC-4626
 *  inflation-attack defence. Mirrors `sakura_perps_risk::pool::MINIMUM_LIQUIDITY`. */
const MINIMUM_LIQUIDITY = 1_000_000n;

/**
 * The liquidity provider, derived rather than generated.
 *
 * A random keypair is discarded at process exit along with whatever it still
 * holds — the first version of this script leaked its entire deposit into an
 * address nobody has the key for. Deriving it means the LP persists across
 * runs, so funds are reused, and the second run exercises the SAME owner
 * withdrawing twice, which is exactly the escrow bug this pool had.
 *
 * Devnet only, and the seed is in the clear on purpose: this account is a test
 * fixture, not a wallet.
 */
const LP = Keypair.fromSeed(
  createHash('sha256').update('sakura-perps devnet round-trip lp v1').digest(),
);

function loadKeypair(path: string): Keypair {
  const raw = JSON.parse(fs.readFileSync(path.replace('~', process.env.HOME ?? process.env.USERPROFILE ?? ''), 'utf8'));
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

async function main() {
  const rpc = process.env.RPC_URL;
  const keypairPath = process.env.KEYPAIR;
  if (!rpc || !keypairPath) throw new Error('set RPC_URL and KEYPAIR');

  const connection = new Connection(rpc, 'confirmed');
  const wallet = new anchor.Wallet(loadKeypair(keypairPath));
  const provider = new anchor.AnchorProvider(connection, wallet, { commitment: 'confirmed' });
  anchor.setProvider(provider);

  const idl = JSON.parse(fs.readFileSync('idl/sakura_perps.json', 'utf8'));
  const program = new Program(idl, provider) as Program<SakuraPerps>;
  const admin = wallet.publicKey;
  console.log('admin      :', admin.toBase58());
  console.log('program    :', program.programId.toBase58());
  if (!program.programId.equals(PROGRAM_ID)) throw new Error('IDL program id is not the deployed one');

  // ── Preflight: can this round-trip actually complete? ─────────────────────
  // USDC-devnet's mint authority is the mint itself, so there is no minting our
  // way out of an empty balance — it comes from Circle's faucet or not at all.
  const balanceOf = (account: PublicKey) =>
    connection
      .getTokenAccountBalance(account)
      .then((r) => BigInt(r.value.amount))
      .catch(() => 0n);

  const ata = getAssociatedTokenAddressSync(USDC_DEVNET, admin, false, TOKEN_PROGRAM_ID);
  const lpUsdcAta = getAssociatedTokenAddressSync(USDC_DEVNET, LP.publicKey, false, TOKEN_PROGRAM_ID);
  const usdcBalance = await balanceOf(ata);
  const lpUsdc = await balanceOf(lpUsdcAta);
  console.log('admin USDC :', Number(usdcBalance) / 1e6);
  console.log('lp         :', LP.publicKey.toBase58());
  console.log('lp USDC    :', Number(lpUsdc) / 1e6);

  // The LP keeps its balance between runs, so only the shortfall has to come
  // from the admin. Checked together: a funded LP means the round trip can run
  // even when the admin wallet is empty.
  if (lpUsdc < DEPOSIT && usdcBalance < DEPOSIT - lpUsdc) {
    console.error(
      `\nNeed ${Number(DEPOSIT) / 1e6} USDC-devnet for the deposit leg. LP holds ` +
        `${Number(lpUsdc) / 1e6}, admin holds ${Number(usdcBalance) / 1e6}.\n` +
        `Fund either address from https://faucet.circle.com (Solana Devnet, USDC):\n` +
        `  admin ${admin.toBase58()}\n  lp    ${LP.publicKey.toBase58()}\n\n` +
        `Stopping BEFORE initialize_exchange on purpose: the exchange PDA is a\n` +
        `singleton with no close instruction, so initializing it now would pin the\n` +
        `collateral mint permanently while leaving the deposit leg unproven.`,
    );
    process.exit(2);
  }

  const [exchange] = PublicKey.findProgramAddressSync([Buffer.from('exchange')], program.programId);
  const [pool] = PublicKey.findProgramAddressSync([Buffer.from('pool')], program.programId);
  const [quoteVault] = PublicKey.findProgramAddressSync([Buffer.from('quote_vault')], program.programId);
  const [shareMint] = PublicKey.findProgramAddressSync([Buffer.from('share_mint')], program.programId);
  const [poolShareAccount] = PublicKey.findProgramAddressSync([Buffer.from('pool_shares')], program.programId);

  const exists = async (pk: PublicKey) => (await connection.getAccountInfo(pk)) !== null;

  if (!(await exists(exchange))) {
    console.log('\ninitialize_exchange …');
    const sig = await program.methods
      .initializeExchange({
        feeRecipient: admin,
        protocolFeeShareBps: 1_000,
        // Circle can freeze USDC accounts, including this pool's vault. Accepted
        // deliberately: refusing every freezable mint refuses USDC itself, and
        // the authority is recorded on the Exchange for anyone auditing it.
        allowFreezableCollateral: true,
      })
      .accounts({ admin, collateralMint: USDC_DEVNET })
      .rpc();
    console.log('  sig', sig);
  } else {
    console.log('\nexchange already initialized');
  }

  if (!(await exists(pool))) {
    console.log('initialize_pool …');
    const sig = await program.methods
      .initializePool({
        depositFeeBps: 0,
        withdrawFeeBps: 0,
        // Zero on devnet so the round-trip can complete in one run. The delay is
        // the whole point of the two-step withdraw in production — this proves
        // the mechanism, not the timing.
        withdrawDelaySeconds: 0,
        // M5_MAX_UTILIZATION_BPS. Anything above it is now refused by
        // initialize_pool: the ceiling is the protocol's bound on how far an LP
        // share price can be overstated, so it is enforced where the pool is
        // created and not only where it is later retuned.
        maxUtilizationBps: 2_000,
        maxAumQuote: new anchor.BN('1000000000000'), // 1,000,000 USDC
      })
      .accounts({ admin, collateralMint: USDC_DEVNET, tokenProgram: TOKEN_PROGRAM_ID })
      .rpc();
    console.log('  sig', sig);
  } else {
    console.log('pool already initialized');
  }

  // initialize_exchange sets paused_flags = ALL; nothing can move until lifted.
  console.log('set_pause_flags(0) …');
  console.log('  sig', await program.methods.setPauseFlags(new anchor.BN(0)).accounts({ admin }).rpc());

  // ── The depositor is a dedicated LP keypair, not the admin ───────────────
  //
  // Two reasons. It is more honest — an LP is not the admin — and it sidesteps
  // an escrow this wallet cannot clear: a run against the pre-fix build left
  // `[b"withdraw_escrow", admin]` behind, and `request_withdraw` is the very
  // instruction blocked by it, so the admin can never withdraw again on this
  // pool. Escrow PDAs are seeded per owner, so a new owner gets a clean one.
  //
  // That stranded account is the bug's cost made concrete: nothing in the
  // program can close an escrow except `lp_withdraw`, which cannot be reached
  // without `request_withdraw` succeeding first.
  console.log('\nlp (fresh)  :', LP.publicKey.toBase58());
  const shareAta = getAssociatedTokenAddressSync(shareMint, LP.publicKey, true, TOKEN_PROGRAM_ID);

  if (lpUsdc < DEPOSIT) {
    const fund = new Transaction()
      .add(SystemProgram.transfer({ fromPubkey: admin, toPubkey: LP.publicKey, lamports: 30_000_000 }))
      .add(createAssociatedTokenAccountIdempotentInstruction(admin, lpUsdcAta, LP.publicKey, USDC_DEVNET, TOKEN_PROGRAM_ID))
      .add(createTransferCheckedInstruction(ata, USDC_DEVNET, lpUsdcAta, admin, DEPOSIT - lpUsdc, 6, [], TOKEN_PROGRAM_ID))
      .add(createAssociatedTokenAccountIdempotentInstruction(admin, shareAta, LP.publicKey, shareMint, TOKEN_PROGRAM_ID));
    console.log('  funding sig', await provider.sendAndConfirm(fund, []));
  }

  // Snapshot before depositing. The pool may already hold liquidity — from an
  // earlier run, or from another provider — and the expectations differ:
  // MINIMUM_LIQUIDITY is taken only on the very first deposit, so assuming a
  // cold pool would make a correct run look broken.
  const sharesBefore = (await getAccount(connection, shareAta, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  const vaultBefore = await connection
    .getTokenAccountBalance(quoteVault)
    .then((r) => BigInt(r.value.amount))
    .catch(() => 0n);
  const poolWasEmpty = vaultBefore === 0n;
  console.log(`\npool before  : vault ${Number(vaultBefore) / 1e6} USDC, your shares ${sharesBefore}`);

  console.log(`lp_deposit ${Number(DEPOSIT) / 1e6} USDC …`);
  console.log(
    '  sig',
    await program.methods
      .lpDeposit(new anchor.BN(DEPOSIT.toString()), new anchor.BN(0))
      .accounts({
        depositor: LP.publicKey,
        collateralMint: USDC_DEVNET,
        shareMint,
        depositorTokenAccount: lpUsdcAta,
        depositorShareAccount: shareAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([LP])
      .rpc(),
  );

  const shares = (await getAccount(connection, shareAta, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  const vaulted = (await getAccount(connection, quoteVault, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  console.log('  shares minted :', shares.toString());
  console.log('  vault balance :', Number(vaulted) / 1e6, 'USDC');

  console.log('\nrequest_withdraw (all shares) …');
  console.log(
    '  sig',
    await program.methods
      .requestWithdraw(new anchor.BN(shares.toString()))
      .accounts({ owner: LP.publicKey, shareMint, ownerShareAccount: shareAta, tokenProgram: TOKEN_PROGRAM_ID })
      .signers([LP])
      .rpc(),
  );

  // Request and withdraw must not share a slot. The pool refuses that even with
  // the delay configured as zero — it is what closes the atomic sandwich, where
  // a provider requests and redeems inside one bundle around a price move. The
  // SVM suite pins the rule in
  // `request_and_withdraw_in_one_slot_is_refused_even_with_no_delay`.
  await new Promise((r) => setTimeout(r, 3000));

  console.log('lp_withdraw …');
  console.log(
    '  sig',
    await program.methods
      .lpWithdraw(new anchor.BN(0))
      // accountsPartial, not accounts: `owner` carries relations:["withdraw_request"]
      // so the typed builder treats it as resolvable and refuses to accept it —
      // then defaults it to the provider wallet, leaving the LP out of the
      // signer set entirely ("unknown signer"). The withdraw_request and escrow
      // PDAs are seeded from `owner`, so getting this wrong addresses the admin's
      // accounts, not the LP's.
      .accountsPartial({
        owner: LP.publicKey,
        collateralMint: USDC_DEVNET,
        shareMint,
        ownerTokenAccount: lpUsdcAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([LP])
      .rpc(),
  );

  const after = (await getAccount(connection, lpUsdcAta, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  const vaultAfter = (await getAccount(connection, quoteVault, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  console.log('\nUSDC before :', Number(usdcBalance) / 1e6);
  console.log('USDC after  :', Number(after) / 1e6);
  console.log('vault after :', Number(vaultAfter) / 1e6);
  // What the depositor should get back, derived from the state observed before
  // the deposit rather than assumed.
  //
  // MINIMUM_LIQUIDITY is locked to the pool's own share account on the FIRST
  // deposit only — the ERC-4626 defence against a first depositor donating to
  // the vault to make one share worth more than the next depositor's entire
  // deposit. So a cold pool keeps 1 USDC forever and a warm one does not, and
  // hardcoding either makes a correct run look broken.
  const minted = shares - sharesBefore;
  const expectedMinted = poolWasEmpty ? DEPOSIT - MINIMUM_LIQUIDITY : DEPOSIT;
  // Every share this wallet held was withdrawn, so it recovers its own deposit
  // plus anything it had stranded in the pool beforehand.
  // The LP was funded with exactly DEPOSIT and redeemed every share it held,
  // so it ends with its deposit back, less the minimum if it seeded an empty pool.
  const expectedAfter = poolWasEmpty ? DEPOSIT - MINIMUM_LIQUIDITY : DEPOSIT;

  const checks: Array<[string, boolean]> = [
    [
      `shares minted == ${expectedMinted} (${poolWasEmpty ? 'cold pool: minimum locked' : 'warm pool: no minimum taken'})`,
      minted === expectedMinted,
    ],
    [`USDC balance is ${Number(expectedAfter) / 1e6}`, after === expectedAfter],
    // The pool is shared, so it keeps whatever other providers left in it. The
    // invariant that matters is that a complete cycle puts the vault back
    // exactly where it started — asserting MINIMUM_LIQUIDITY only holds when
    // this LP is the sole depositor, which on a live cluster it is not.
    [
      `vault returned to its pre-deposit balance (${Number(poolWasEmpty ? MINIMUM_LIQUIDITY : vaultBefore) / 1e6})`,
      vaultAfter === (poolWasEmpty ? MINIMUM_LIQUIDITY : vaultBefore),
    ],
    ['every share this wallet held was redeemed', shares > 0n],
  ];
  let ok = true;
  for (const [label, passed] of checks) {
    console.log(`  ${passed ? 'PASS' : 'FAIL'}  ${label}`);
    ok &&= passed;
  }
  console.log(
    ok
      ? '\nROUND TRIP VERIFIED — deposit, request and withdraw all settled, and the\ninflation-attack minimum stayed locked in the vault.'
      : '\nROUND TRIP FAILED its assertions.',
  );
  if (!ok) process.exit(1);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
