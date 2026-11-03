import assert from "node:assert/strict";
import test from "node:test";
import { Keypair, PublicKey } from "@solana/web3.js";
import {
  getAssociatedTokenAddressSync,
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
} from "@solana/spl-token";
import { associatedTokenAddress } from "../src/token-address";

test("ATA derivation matches official SDK for both token programs and wallet/PDA owners", () => {
  for (let seed = 0; seed < 32; seed++) {
    const owner = Keypair.fromSeed(new Uint8Array(32).fill(seed)).publicKey;
    const mint = Keypair.fromSeed(new Uint8Array(32).fill(seed + 33)).publicKey;
    const [pda] = PublicKey.findProgramAddressSync([Buffer.from([seed])], mint);
    for (const candidate of [owner, pda]) {
      for (const program of [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID]) {
        assert.equal(
          associatedTokenAddress(mint, candidate, program).toBase58(),
          getAssociatedTokenAddressSync(mint, candidate, true, program).toBase58(),
        );
      }
      assert.notEqual(
        associatedTokenAddress(mint, candidate, TOKEN_PROGRAM_ID).toBase58(),
        associatedTokenAddress(mint, candidate, TOKEN_2022_PROGRAM_ID).toBase58(),
      );
    }
  }
});
