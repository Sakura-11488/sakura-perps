//! The freezable-collateral guard, both ways.
//!
//! `initialize_exchange` refuses a collateral mint carrying a freeze authority
//! unless the admin opts in. Before this file the error existed and nothing
//! exercised it — a guard with no test is a guard you find out about in
//! production, which is exactly what happened: it was discovered by a live
//! devnet call failing against real Circle USDC.
//!
//! The mint is written as raw account bytes rather than built with
//! `litesvm_token::CreateMint`, because that builder exposes no freeze
//! authority. The SPL mint layout is fixed and documented, and this crate
//! already reads token accounts the same way.

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use sakura_perps::{Exchange, InitializeExchangeParams, PerpsError};
use solana_account::Account;
use solana_address::Address;
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const COLLATERAL_DECIMALS: u8 = 6;
/// `Mint::LEN` for the legacy SPL Token program.
const MINT_LEN: usize = 82;

fn spl_token_id() -> Address {
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        .parse()
        .unwrap()
}

fn program_binary() -> Vec<u8> {
    let path = std::env::var("SAKURA_PERPS_SO")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/deploy/sakura_perps.so")
        });
    std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read the program binary at {}: {err}\n\
             Build it, or point SAKURA_PERPS_SO at an existing .so. These tests \
             deliberately do not skip.",
            path.display()
        )
    })
}

/// Legacy SPL Token mint layout:
/// `COption<Pubkey>` authority (4 + 32), supply u64, decimals u8,
/// is_initialized bool, then `COption<Pubkey>` freeze authority (4 + 32).
fn mint_bytes(mint_authority: &Address, freeze_authority: Option<&Address>) -> Vec<u8> {
    let mut data = vec![0u8; MINT_LEN];
    data[0..4].copy_from_slice(&1u32.to_le_bytes()); // Some(mint_authority)
    data[4..36].copy_from_slice(mint_authority.as_ref());
    data[36..44].copy_from_slice(&0u64.to_le_bytes()); // supply
    data[44] = COLLATERAL_DECIMALS;
    data[45] = 1; // is_initialized
    match freeze_authority {
        Some(key) => {
            data[46..50].copy_from_slice(&1u32.to_le_bytes());
            data[50..82].copy_from_slice(key.as_ref());
        }
        None => data[46..50].copy_from_slice(&0u32.to_le_bytes()),
    }
    data
}

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    mint: Address,
    exchange: Address,
}

/// A fresh SVM with a collateral mint that either can or cannot be frozen.
fn env(freezable: bool) -> Env {
    let mut svm = LiteSVM::new();
    svm.add_program(sakura_perps::ID, &program_binary())
        .expect("program loads");

    let admin = Keypair::new();
    svm.airdrop(&admin.pubkey(), 100 * 1_000_000_000).unwrap();

    let mint = Keypair::new().pubkey();
    let freeze_authority = freezable.then(|| admin.pubkey());
    svm.set_account(
        mint,
        Account {
            lamports: 1_000_000_000,
            data: mint_bytes(&admin.pubkey(), freeze_authority.as_ref()),
            owner: spl_token_id(),
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("write mint");

    let exchange = Address::find_program_address(&[b"exchange"], &sakura_perps::ID).0;
    Env {
        svm,
        admin,
        mint,
        exchange,
    }
}

fn initialize(env: &mut Env, allow_freezable: bool) -> Result<(), TransactionError> {
    let instruction = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::InitializeExchange {
            admin: env.admin.pubkey(),
            exchange: env.exchange,
            collateral_mint: env.mint,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::InitializeExchange {
            params: InitializeExchangeParams {
                fee_recipient: env.admin.pubkey(),
                protocol_fee_share_bps: 1_000,
                allow_freezable_collateral: allow_freezable,
            },
        }
        .data(),
    };
    let blockhash = env.svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&env.admin.pubkey()),
        &[&env.admin],
        blockhash,
    );
    env.svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

fn exchange_state(env: &Env) -> Exchange {
    let account = env.svm.get_account(&env.exchange).expect("exchange exists");
    Exchange::try_deserialize(&mut account.data.as_slice()).expect("deserialize exchange")
}

fn expect_error(result: Result<(), TransactionError>, expected: PerpsError) {
    let code = expected as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
    match result {
        Err(TransactionError::InstructionError(0, InstructionError::Custom(actual))) => {
            assert_eq!(actual, code, "expected {expected:?} ({code}), got {actual}");
        }
        other => panic!("expected {expected:?}, got {other:?}"),
    }
}

/// The default path. This is the case real USDC hits, on devnet and mainnet
/// alike — both carry a freeze authority.
#[test]
fn freezable_collateral_is_refused_by_default() {
    let mut env = env(true);
    expect_error(
        initialize(&mut env, false),
        PerpsError::CollateralMintIsFreezable,
    );
    assert!(
        env.svm
            .get_account(&env.exchange)
            .is_none_or(|a| a.data.is_empty()),
        "a refused initialize must not leave an exchange behind",
    );
}

/// Opting in is what makes USDC usable at all, so it has to actually work —
/// and it has to record who holds the freeze power.
#[test]
fn freezable_collateral_is_accepted_when_opted_in() {
    let mut env = env(true);
    initialize(&mut env, true).expect("opt-in accepts a freezable mint");

    let exchange = exchange_state(&env);
    assert_eq!(exchange.collateral_mint, env.mint);
    assert_eq!(
        exchange.collateral_freeze_authority,
        env.admin.pubkey(),
        "the freeze authority must be recorded, not merely tolerated",
    );
}

/// The opt-in must not become the only path: a mint with no freeze authority
/// still succeeds with the flag closed, and records no authority.
#[test]
fn non_freezable_collateral_needs_no_opt_in() {
    let mut env = env(false);
    initialize(&mut env, false).expect("a non-freezable mint needs no opt-in");

    let exchange = exchange_state(&env);
    assert_eq!(
        exchange.collateral_freeze_authority,
        Address::default(),
        "no freeze authority should be recorded as the default pubkey",
    );
}
