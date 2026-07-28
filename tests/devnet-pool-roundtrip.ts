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
import { PublicKey, Keypair, SystemProgram, Connection, SYSVAR_RENT_PUBKEY } from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
  getAccount,
} from '@solana/spl-token';
import fs from 'fs';
import type { SakuraPerps } from '../target/types/sakura_perps';

const USDC_DEVNET = new PublicKey('Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr');
const PROGRAM_ID = new PublicKey('5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y');
/** 10 USDC, six decimals. Small enough to be cheap, large enough that share
 *  maths is not operating on dust. */
const DEPOSIT = 10_000_000n;

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
  const ata = getAssociatedTokenAddressSync(USDC_DEVNET, admin, false, TOKEN_PROGRAM_ID);
  let usdcBalance = 0n;
  try {
    usdcBalance = (await getAccount(connection, ata, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  } catch {
    usdcBalance = 0n;
  }
  console.log('USDC ATA   :', ata.toBase58());
  console.log('USDC balance:', Number(usdcBalance) / 1e6);

  if (usdcBalance < DEPOSIT) {
    console.error(
      `\nNeed at least ${Number(DEPOSIT) / 1e6} USDC-devnet to run the deposit leg; have ` +
        `${Number(usdcBalance) / 1e6}.\n` +
        `Fund ${admin.toBase58()} from https://faucet.circle.com (Solana Devnet, USDC).\n\n` +
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
      .initializeExchange({ feeRecipient: admin, protocolFeeShareBps: 1_000 })
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
        maxUtilizationBps: 8_000,
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

  const shareAta = getAssociatedTokenAddressSync(shareMint, admin, true, TOKEN_PROGRAM_ID);

  console.log(`\nlp_deposit ${Number(DEPOSIT) / 1e6} USDC …`);
  console.log(
    '  sig',
    await program.methods
      .lpDeposit(new anchor.BN(DEPOSIT.toString()), new anchor.BN(0))
      .accounts({
        depositor: admin,
        collateralMint: USDC_DEVNET,
        shareMint,
        depositorTokenAccount: ata,
        depositorShareAccount: shareAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
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
      .accounts({ shareMint, ownerShareAccount: shareAta, tokenProgram: TOKEN_PROGRAM_ID })
      .rpc(),
  );

  console.log('lp_withdraw …');
  console.log(
    '  sig',
    await program.methods
      .lpWithdraw(new anchor.BN(0))
      .accounts({
        collateralMint: USDC_DEVNET,
        shareMint,
        ownerTokenAccount: ata,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc(),
  );

  const after = (await getAccount(connection, ata, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  const vaultAfter = (await getAccount(connection, quoteVault, 'confirmed', TOKEN_PROGRAM_ID)).amount;
  console.log('\nUSDC before :', Number(usdcBalance) / 1e6);
  console.log('USDC after  :', Number(after) / 1e6);
  console.log('vault after :', Number(vaultAfter) / 1e6);
  console.log(
    after === usdcBalance ? '\nROUND TRIP CLEAN — deposited and withdrew the same amount' : '\nNOTE: balance differs from the start; check fees/rounding',
  );
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
