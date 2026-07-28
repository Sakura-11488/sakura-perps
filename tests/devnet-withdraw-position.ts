/**
 * Withdraw the signer's entire LP position on devnet.
 *
 *   npx ts-node tests/devnet-withdraw-position.ts
 *
 * Env: RPC_URL, KEYPAIR.
 *
 * Written to prove `close_stale_escrow` did what it claims. The admin wallet
 * held 15 shares it could not redeem: its escrow was orphaned by the pre-fix
 * `lp_withdraw`, and `request_withdraw` — the instruction that creates that
 * escrow — failed at account creation every time. "The owner can withdraw
 * again" is a claim until this actually returns the collateral.
 */
import * as anchor from '@coral-xyz/anchor';
import { Program } from '@coral-xyz/anchor';
import { PublicKey, Keypair, Connection } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync } from '@solana/spl-token';
import fs from 'fs';
import type { SakuraPerps } from '../target/types/sakura_perps';

const PROGRAM_ID = new PublicKey('5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y');
const USDC_DEVNET = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');

function loadKeypair(path: string): Keypair {
  const raw = JSON.parse(
    fs.readFileSync(path.replace('~', process.env.HOME ?? process.env.USERPROFILE ?? ''), 'utf8'),
  );
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
  const owner = wallet.publicKey;

  const [shareMint] = PublicKey.findProgramAddressSync([Buffer.from('share_mint')], PROGRAM_ID);
  const shareAta = getAssociatedTokenAddressSync(shareMint, owner, true, TOKEN_PROGRAM_ID);
  const usdcAta = getAssociatedTokenAddressSync(USDC_DEVNET, owner, false, TOKEN_PROGRAM_ID);

  const balanceOf = (a: PublicKey) =>
    connection
      .getTokenAccountBalance(a)
      .then((r) => BigInt(r.value.amount))
      .catch(() => 0n);

  const shares = await balanceOf(shareAta);
  const usdcBefore = await balanceOf(usdcAta);
  console.log('owner  :', owner.toBase58());
  console.log('shares :', shares.toString());
  console.log('USDC   :', Number(usdcBefore) / 1e6);
  if (shares === 0n) {
    console.log('\nNo shares to withdraw.');
    return;
  }

  console.log('\nrequest_withdraw …');
  console.log(
    '  sig',
    await program.methods
      .requestWithdraw(new anchor.BN(shares.toString()))
      .accountsPartial({ owner, shareMint, ownerShareAccount: shareAta, tokenProgram: TOKEN_PROGRAM_ID })
      .rpc(),
  );

  // Not the same slot — refused even with a zero delay, by design.
  await new Promise((r) => setTimeout(r, 3000));

  console.log('lp_withdraw …');
  console.log(
    '  sig',
    await program.methods
      .lpWithdraw(new anchor.BN(0))
      .accountsPartial({
        owner,
        collateralMint: USDC_DEVNET,
        shareMint,
        ownerTokenAccount: usdcAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc(),
  );

  const usdcAfter = await balanceOf(usdcAta);
  const sharesAfter = await balanceOf(shareAta);
  console.log('\nUSDC before :', Number(usdcBefore) / 1e6);
  console.log('USDC after  :', Number(usdcAfter) / 1e6);
  console.log('shares after:', sharesAfter.toString());

  const checks: Array<[string, boolean]> = [
    ['collateral was returned', usdcAfter > usdcBefore],
    ['every share was redeemed', sharesAfter === 0n],
  ];
  let ok = true;
  for (const [label, passed] of checks) {
    console.log(`  ${passed ? 'PASS' : 'FAIL'}  ${label}`);
    ok &&= passed;
  }
  console.log(
    ok
      ? `\nPOSITION RECOVERED — ${Number(usdcAfter - usdcBefore) / 1e6} USDC returned to a wallet that could not withdraw at all before close_stale_escrow.`
      : '\nFAILED its assertions.',
  );
  if (!ok) process.exit(1);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
