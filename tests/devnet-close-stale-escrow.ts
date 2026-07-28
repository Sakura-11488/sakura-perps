/**
 * Clear an orphaned withdraw escrow on devnet.
 *
 *   npx ts-node tests/devnet-close-stale-escrow.ts
 *
 * Env: RPC_URL, KEYPAIR — the keypair is the escrow's owner, since only they
 * can sign for it and only they get the rent back.
 *
 * There is a real account in this state: the admin wallet ran the round-trip
 * against the build whose `lp_withdraw` closed the WithdrawRequest but not its
 * escrow. Because `request_withdraw` *creates* that escrow, the admin has been
 * unable to withdraw from this pool since — the failure `close_stale_escrow`
 * exists to undo. Recovering it is the only honest proof the instruction works,
 * as opposed to a fixture that simulates the shape.
 */
import * as anchor from '@coral-xyz/anchor';
import { Program } from '@coral-xyz/anchor';
import { PublicKey, Keypair, Connection } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';
import fs from 'fs';
import type { SakuraPerps } from '../target/types/sakura_perps';

const PROGRAM_ID = new PublicKey('5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y');

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

  const [escrow] = PublicKey.findProgramAddressSync(
    [Buffer.from('withdraw_escrow'), owner.toBuffer()],
    PROGRAM_ID,
  );
  const [request] = PublicKey.findProgramAddressSync(
    [Buffer.from('withdraw_request'), owner.toBuffer()],
    PROGRAM_ID,
  );

  console.log('owner   :', owner.toBase58());
  console.log('escrow  :', escrow.toBase58());
  console.log('request :', request.toBase58());

  const escrowInfo = await connection.getAccountInfo(escrow);
  if (!escrowInfo) {
    console.log('\nNo escrow at that address — nothing to recover.');
    return;
  }
  const escrowBalance = await connection
    .getTokenAccountBalance(escrow)
    .then((r) => BigInt(r.value.amount))
    .catch(() => 0n);
  const requestInfo = await connection.getAccountInfo(request);
  const lamportsBefore = (await connection.getAccountInfo(owner))?.lamports ?? 0;

  console.log('\nescrow shares  :', escrowBalance.toString());
  console.log('request exists :', requestInfo !== null);
  console.log('escrow rent    :', escrowInfo.lamports / 1e9, 'SOL');

  // Mirror the on-chain guards, so a refusal is explained here rather than
  // arriving as a bare custom error code.
  if (escrowBalance > 0n) {
    console.error('\nEscrow still holds shares — this is a live request, not an orphan.');
    console.error('Complete it with lp_withdraw instead; close_stale_escrow will refuse.');
    process.exit(2);
  }
  if (requestInfo !== null) {
    console.error('\nA withdraw request is still open, so the escrow is load-bearing.');
    console.error('close_stale_escrow will refuse: closing it would strand that request.');
    process.exit(2);
  }

  console.log('\nclose_stale_escrow …');
  const sig = await program.methods
    .closeStaleEscrow()
    .accountsPartial({ owner, tokenProgram: TOKEN_PROGRAM_ID })
    .rpc();
  console.log('  sig', sig);

  const after = await connection.getAccountInfo(escrow);
  const lamportsAfter = (await connection.getAccountInfo(owner))?.lamports ?? 0;
  const recovered = (lamportsAfter - lamportsBefore) / 1e9;

  const checks: Array<[string, boolean]> = [
    ['escrow account is gone', after === null || after.data.length === 0],
    ['rent was returned to the owner', lamportsAfter > lamportsBefore],
  ];
  let ok = true;
  for (const [label, passed] of checks) {
    console.log(`  ${passed ? 'PASS' : 'FAIL'}  ${label}`);
    ok &&= passed;
  }
  console.log(`\nrent recovered: ${recovered.toFixed(6)} SOL (net of fees)`);
  console.log(
    ok
      ? 'RECOVERED — this owner can request a withdrawal again.'
      : 'FAILED its assertions.',
  );
  if (!ok) process.exit(1);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
