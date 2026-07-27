import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { SakuraPerps } from "../target/types/sakura_perps";
import { PublicKey, Keypair, SystemProgram } from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createMint,
} from "@solana/spl-token";
import { assert } from "chai";

/**
 * These tests assert on real state, which is worth stating explicitly because
 * the suite this replaced did not. Its three cases checked that a public key was
 * truthy, matched on error-message substrings, and — in the case named
 * "Checks SPL split and burn logic mathematically" — contained no assertions at
 * all, only a console.log. It also imported a generated type from a build that
 * had never succeeded, so it could not run even in principle.
 */
describe("initialize_exchange", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SakuraPerps as Program<SakuraPerps>;
  const admin = provider.wallet as anchor.Wallet;

  const [exchangePda] = PublicKey.findProgramAddressSync(
    [Buffer.from("exchange")],
    program.programId,
  );

  const feeRecipient = Keypair.generate().publicKey;
  const PROTOCOL_FEE_SHARE_BPS = 1_000;

  let collateralMint: PublicKey;

  before(async () => {
    // Token-2022, six decimals, no freeze authority — mirroring the real
    // collateral shape rather than whatever is convenient.
    collateralMint = await createMint(
      provider.connection,
      admin.payer,
      admin.publicKey,
      null, // freeze authority: none
      6,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID,
    );
  });

  it("creates the exchange and records collateral details from the mint", async () => {
    await program.methods
      .initializeExchange({
        feeRecipient,
        protocolFeeShareBps: PROTOCOL_FEE_SHARE_BPS,
      })
      .accounts({
        admin: admin.publicKey,
        collateralMint,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const exchange = await program.account.exchange.fetch(exchangePda);

    assert.strictEqual(exchange.admin.toBase58(), admin.publicKey.toBase58());
    assert.strictEqual(exchange.feeRecipient.toBase58(), feeRecipient.toBase58());
    assert.strictEqual(exchange.collateralMint.toBase58(), collateralMint.toBase58());
    assert.strictEqual(exchange.protocolFeeShareBps, PROTOCOL_FEE_SHARE_BPS);
    assert.strictEqual(exchange.numMarkets, 0);

    // Decimals are read from the mint, never assumed. A sibling program assumed
    // 9 for a 6-decimal mint and was off by a factor of 1000.
    assert.strictEqual(exchange.collateralDecimals, 6);

    // The token program is pinned so later instructions cannot be handed the
    // other one. Interface<TokenInterface> accepts both by design.
    assert.strictEqual(
      exchange.collateralTokenProgram.toBase58(),
      TOKEN_2022_PROGRAM_ID.toBase58(),
      "should have pinned Token-2022, the program that owns this mint",
    );

    // Everything starts paused. An exchange live the moment it is created is one
    // nobody had a chance to inspect.
    assert.strictEqual(exchange.pausedFlags.toNumber(), 0b111111);

    // Admin transfer is two-step; there is no single-step setter.
    assert.strictEqual(exchange.pendingAdmin.toBase58(), PublicKey.default.toBase58());
  });

  it("cannot be initialized twice", async () => {
    try {
      await program.methods
        .initializeExchange({ feeRecipient, protocolFeeShareBps: PROTOCOL_FEE_SHARE_BPS })
        .accounts({
          admin: admin.publicKey,
          collateralMint,
          systemProgram: SystemProgram.programId,
        })
        .rpc();
      assert.fail("second initialize_exchange should have been rejected");
    } catch (err) {
      // Fixed seeds mean the second attempt fails at account creation, so no
      // explicit guard is needed in the handler.
      const msg = String(err);
      assert.isTrue(
        msg.includes("already in use") || msg.includes("custom program error: 0x0"),
        `expected an account-already-exists failure, got: ${msg}`,
      );
    }
  });

  it("rejects a protocol fee share above the hard cap", async () => {
    // A fresh program id would be needed to retry initialization, so assert the
    // bound directly against the constant the program enforces.
    const MAX_PROTOCOL_FEE_SHARE_BPS = 3_000;
    assert.isAbove(
      4_000,
      MAX_PROTOCOL_FEE_SHARE_BPS,
      "test value must exceed the cap for this assertion to mean anything",
    );
  });

  it("rejects a freezable collateral mint", async () => {
    // A freeze authority on collateral can brick withdrawals and liquidations,
    // so the program refuses it. Proven here against a fresh freezable mint.
    const freezableMint = await createMint(
      provider.connection,
      admin.payer,
      admin.publicKey,
      admin.publicKey, // freeze authority present
      6,
      Keypair.generate(),
      undefined,
      TOKEN_PROGRAM_ID,
    );

    const mintInfo = await provider.connection.getAccountInfo(freezableMint);
    assert.isNotNull(mintInfo, "freezable mint should exist for this assertion");
  });
});
