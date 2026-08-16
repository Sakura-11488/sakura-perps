//! In-process SVM tests for the collateral vault.
//!
//! The share maths is already property-tested in the risk crate. What is tested
//! here is everything that only exists on chain: that tokens actually move, that
//! the solvency invariant holds after they do, that access control refuses the
//! specific things it claims to, and — the headline — that donating to the vault
//! cannot move the share price.
//!
//! Runs against a **legacy SPL Token** mint, matching devnet USDC. That is the
//! more awkward of the two cases, because `Interface<TokenInterface>` accepts
//! either program and the exchange has to pin the right one.

use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use litesvm_token::{CreateAssociatedTokenAccount, CreateMint, MintTo};
use sakura_perps::pool::{InitializePoolParams, Pool};
use sakura_perps::{InitializeExchangeParams, PerpsError};
use solana_address::Address;
use solana_clock::Clock;
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

/// Legacy SPL Token program. Parsed rather than imported from `spl-token`,
/// whose `Pubkey` comes from a different `solana-pubkey` version and therefore
/// is not the same type as everything else here.
fn spl_token_id() -> Address {
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        .parse()
        .unwrap()
}

/// Token-2022, for the wrong-program test.
fn token_2022_id() -> Address {
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        .parse()
        .unwrap()
}

fn rent_sysvar_id() -> Address {
    "SysvarRent111111111111111111111111111111111"
        .parse()
        .unwrap()
}

const COLLATERAL_DECIMALS: u8 = 6;
/// One whole collateral token.
const ONE: u64 = 1_000_000;

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

/// Everything a test needs, with the exchange and pool already initialised.
struct Fixture {
    svm: LiteSVM,
    admin: Keypair,
    lp: Keypair,
    collateral_mint: Address,
    pool: Address,
    quote_vault: Address,
    share_mint: Address,
    pool_share_account: Address,
    lp_token_account: Address,
    lp_share_account: Address,
    exchange: Address,
}

fn pda(seeds: &[&[u8]]) -> Address {
    Address::find_program_address(seeds, &sakura_perps::ID).0
}

impl Fixture {
    fn new(params: InitializePoolParams) -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(sakura_perps::ID, &program_binary())
            .expect("program loads");

        let admin = Keypair::new();
        let lp = Keypair::new();
        svm.airdrop(&admin.pubkey(), 100 * 1_000_000_000).unwrap();
        svm.airdrop(&lp.pubkey(), 100 * 1_000_000_000).unwrap();

        // Legacy SPL Token, matching devnet USDC. No freeze authority: the
        // exchange refuses a freezable collateral mint, because its freeze
        // authority could brick withdrawals and liquidations at will.
        let collateral_mint = CreateMint::new(&mut svm, &admin)
            .decimals(COLLATERAL_DECIMALS)
            .send()
            .expect("collateral mint");

        let lp_token_account = CreateAssociatedTokenAccount::new(&mut svm, &lp, &collateral_mint)
            .owner(&lp.pubkey())
            .send()
            .expect("lp token account");

        MintTo::new(
            &mut svm,
            &admin,
            &collateral_mint,
            &lp_token_account,
            1_000_000 * ONE,
        )
        .send()
        .expect("fund lp");

        let exchange = pda(&[b"exchange"]);
        let pool = pda(&[b"pool"]);
        let quote_vault = pda(&[b"quote_vault"]);
        let share_mint = pda(&[b"share_mint"]);
        let pool_share_account = pda(&[b"pool_shares"]);

        let mut fixture = Self {
            svm,
            admin,
            lp,
            collateral_mint,
            pool,
            quote_vault,
            share_mint,
            pool_share_account,
            lp_token_account,
            lp_share_account: Address::default(),
            exchange,
        };

        fixture.initialize_exchange();
        fixture.initialize_pool(params);
        // The exchange is created with every flag set. Nothing could deposit
        // until set_pause_flags existed -- writing this fixture is what found it.
        fixture.set_pause_flags(0);

        // The share mint only exists after the pool is initialised.
        fixture.lp_share_account =
            CreateAssociatedTokenAccount::new(&mut fixture.svm, &fixture.lp, &fixture.share_mint)
                .owner(&fixture.lp.pubkey())
                .send()
                .expect("lp share account");

        fixture
    }

    fn send(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<(), TransactionError> {
        let payer = signers[0];
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            signers,
            self.svm.latest_blockhash(),
        );
        self.svm
            .send_transaction(transaction)
            .map(|_| ())
            .map_err(|failed| failed.err)
    }

    fn initialize_exchange(&mut self) {
        let instruction = Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::InitializeExchange {
                admin: self.admin.pubkey(),
                exchange: self.exchange,
                collateral_mint: self.collateral_mint,
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::InitializeExchange {
                params: InitializeExchangeParams {
                    fee_recipient: self.admin.pubkey(),
                    protocol_fee_share_bps: 1_000,
                    // This fixture's mint has no freeze authority, so the opt-in
                    // is not needed to pass — leaving it closed keeps the guard
                    // under test on the default path.
                    allow_freezable_collateral: false,
                },
            }
            .data(),
        };
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
            .expect("initialize exchange");
    }

    fn set_pause_flags(&mut self, flags: u64) {
        let instruction = Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::SetPauseFlags {
                admin: self.admin.pubkey(),
                exchange: self.exchange,
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::SetPauseFlags { flags }.data(),
        };
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin]).expect("set pause flags");
    }

    fn initialize_pool(&mut self, params: InitializePoolParams) {
        let instruction = self.initialize_pool_ix(params, self.admin.pubkey());
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin]).expect("initialize pool");
    }

    fn initialize_pool_ix(&self, params: InitializePoolParams, admin: Address) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::InitializePool {
                admin,
                exchange: self.exchange,
                pool: self.pool,
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                share_mint: self.share_mint,
                pool_share_account: self.pool_share_account,
                token_program: spl_token_id(),
                system_program: anchor_lang::system_program::ID,
                rent: rent_sysvar_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::InitializePool { params }.data(),
        }
    }

    fn deposit_ix(&self, amount: u64, min_shares_out: u64) -> Instruction {
        Instruction {
            program_id: sakura_perps::ID,
            accounts: sakura_perps::accounts::LpDeposit {
                depositor: self.lp.pubkey(),
                exchange: self.exchange,
                pool: self.pool,
                collateral_mint: self.collateral_mint,
                quote_vault: self.quote_vault,
                share_mint: self.share_mint,
                depositor_token_account: self.lp_token_account,
                depositor_share_account: self.lp_share_account,
                pool_share_account: self.pool_share_account,
                token_program: spl_token_id(),
            }
            .to_account_metas(None),
            data: sakura_perps::instruction::LpDeposit {
                amount,
                min_shares_out,
            }
            .data(),
        }
    }

    fn deposit(&mut self, amount: u64, min_shares_out: u64) -> Result<(), TransactionError> {
        let instruction = self.deposit_ix(amount, min_shares_out);
        let lp = self.lp.insecure_clone();
        self.send(instruction, &[&lp])
    }

    fn pool_state(&self) -> Pool {
        let account = self.svm.get_account(&self.pool).expect("pool exists");
        // Skip the 8-byte Anchor discriminator.
        anchor_lang::AnchorDeserialize::deserialize(&mut &account.data[8..]).expect("pool decodes")
    }

    fn token_balance(&self, account: Address) -> u64 {
        // SPL token account layout: mint(32), owner(32), amount(8, little
        // endian). Read directly rather than pulling in a token crate whose
        // `Pubkey` belongs to a different `solana-pubkey` version — that is
        // exactly what made the previous attempt fail to compile.
        let raw = self
            .svm
            .get_account(&account)
            .expect("token account exists");
        u64::from_le_bytes(
            raw.data[64..72]
                .try_into()
                .expect("token account is long enough to hold an amount"),
        )
    }

    /// Move the cluster clock forward so a withdrawal delay can elapse.
    fn advance(&mut self, seconds: i64, slots: u64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp += seconds;
        clock.slot += slots;
        self.svm.set_sysvar(&clock);
    }
}

fn default_params() -> InitializePoolParams {
    InitializePoolParams {
        deposit_fee_bps: 0,
        withdraw_fee_bps: 0,
        withdraw_delay_seconds: 60,
        // Was 8 000. Stage 3 makes `M5_MAX_UTILIZATION_BPS` the protocol's bound
        // on how far an LP share price can be overstated, and enforces it at
        // `initialize_pool` as well as at `set_pool_limits` — a ceiling settable
        // by one instruction and not the other is not a bound.
        max_utilization_bps: sakura_perps::M5_MAX_UTILIZATION_BPS,
        max_aum_quote: 1_000_000 * ONE,
    }
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

/// The first deposit mints shares, locks the minimum forever, and leaves the
/// vault solvent.
#[test]
fn a_first_deposit_mints_shares_and_locks_the_minimum() {
    let mut fixture = Fixture::new(default_params());

    fixture.deposit(1_000 * ONE, 1).expect("deposit succeeds");

    let pool = fixture.pool_state();
    assert_eq!(pool.quote_deposited, 1_000 * ONE);
    assert_eq!(fixture.token_balance(fixture.quote_vault), 1_000 * ONE);

    // The locked minimum went to the pool's own share account, where nothing
    // can redeem it. This is what stops the share price being movable by orders
    // of magnitude with a dust deposit.
    let locked = fixture.token_balance(fixture.pool_share_account);
    assert_eq!(locked, sakura_perps_risk::pool::MINIMUM_LIQUIDITY as u64);

    let to_lp = fixture.token_balance(fixture.lp_share_account);
    assert_eq!(to_lp, 1_000 * ONE - locked);
    assert_eq!(pool.total_shares, to_lp + locked);
}

/// **Donating to the vault does not move the share price.**
///
/// The ERC-4626 inflation attack, on Solana. If AUM were read from
/// `quote_vault.amount` this test would fail: the donation would inflate the
/// price and the second depositor would receive fewer shares — or none.
#[test]
fn a_donation_to_the_vault_cannot_move_the_share_price() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("first deposit");

    let shares_before = fixture.token_balance(fixture.lp_share_account);
    let deposited_before = fixture.pool_state().quote_deposited;

    // Transfer straight into the vault, bypassing the program entirely.
    MintTo::new(
        &mut fixture.svm,
        &fixture.admin.insecure_clone(),
        &fixture.collateral_mint,
        &fixture.quote_vault,
        500_000 * ONE,
    )
    .send()
    .expect("donation lands in the vault");

    assert!(
        fixture.token_balance(fixture.quote_vault) > deposited_before,
        "the donation should be visible in the raw balance"
    );
    assert_eq!(
        fixture.pool_state().quote_deposited,
        deposited_before,
        "tracked equity must ignore the donation"
    );

    // A second deposit of the same size must mint the same number of shares as
    // the first did, to within the locked minimum.
    //
    // The blockhash has to move first. An identical amount from the same signer
    // against the same accounts produces a byte-identical transaction, which the
    // runtime rejects as `AlreadyProcessed` — nothing to do with the pool.
    fixture.svm.expire_blockhash();
    fixture.deposit(1_000 * ONE, 1).expect("second deposit");

    let minted = fixture.token_balance(fixture.lp_share_account) - shares_before;
    assert_eq!(
        minted,
        1_000 * ONE,
        "the donation moved the share price: minted {minted} instead of {}",
        1_000 * ONE
    );
}

/// A full round trip returns no more than went in.
#[test]
fn a_deposit_and_withdrawal_round_trip_returns_no_more_than_it_took() {
    let mut fixture = Fixture::new(default_params());

    let before = fixture.token_balance(fixture.lp_token_account);
    fixture.deposit(1_000 * ONE, 1).expect("deposit");

    let shares = fixture.token_balance(fixture.lp_share_account);
    request_withdraw(&mut fixture, shares).expect("request");
    fixture.advance(61, 2);
    lp_withdraw(&mut fixture, 1).expect("withdraw");

    let after = fixture.token_balance(fixture.lp_token_account);
    assert!(
        after <= before,
        "round trip returned more than it took: {before} -> {after}"
    );

    // The permanently locked minimum is the difference, and it stays behind.
    let pool = fixture.pool_state();
    assert_eq!(
        pool.total_shares,
        sakura_perps_risk::pool::MINIMUM_LIQUIDITY as u64
    );
}

/// A provider can withdraw more than once.
///
/// `request_withdraw` creates two accounts and `lp_withdraw` closed only one of
/// them, leaving the escrow token account at `[b"withdraw_escrow", owner]`
/// behind. The second request then failed at account creation — "already in
/// use" — and no instruction in the program could clear it, so a provider got
/// exactly one withdrawal and the remainder of their position was stranded.
///
/// Every existing test stopped after a single cycle, which is exactly why the
/// suite stayed green while that shipped. The devnet round-trip found it on its
/// second run.
#[test]
fn a_provider_can_withdraw_twice() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");

    // First cycle: take out half.
    let half = fixture.token_balance(fixture.lp_share_account) / 2;
    request_withdraw(&mut fixture, half).expect("first request");
    fixture.advance(61, 2);
    lp_withdraw(&mut fixture, 1).expect("first withdraw");

    // The escrow must be gone, or the next request cannot allocate it.
    let escrow = pda(&[b"withdraw_escrow", fixture.lp.pubkey().as_ref()]);
    assert!(
        fixture
            .svm
            .get_account(&escrow)
            .is_none_or(|a| a.data.is_empty()),
        "lp_withdraw must close the escrow token account, not just the request",
    );

    // Second cycle: the rest. This is the call that used to fail outright.
    //
    // Fresh blockhash first, or the two requests are byte-identical
    // transactions and the second is rejected as AlreadyProcessed — a
    // deduplication artifact that would masquerade as the bug under test.
    fixture.svm.expire_blockhash();
    let rest = fixture.token_balance(fixture.lp_share_account);
    request_withdraw(&mut fixture, rest).expect("second request must not collide with the first");
    fixture.advance(61, 2);
    lp_withdraw(&mut fixture, 1).expect("second withdraw");

    let pool = fixture.pool_state();
    assert_eq!(
        pool.total_shares,
        sakura_perps_risk::pool::MINIMUM_LIQUIDITY as u64,
        "only the permanently locked minimum should remain",
    );
}

/// Withdrawal before the delay has elapsed is refused.
#[test]
fn a_withdrawal_before_the_delay_is_refused() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");

    let shares = fixture.token_balance(fixture.lp_share_account);
    request_withdraw(&mut fixture, shares).expect("request");

    // A slot passes, but not the sixty seconds.
    fixture.advance(5, 1);
    expect_error(lp_withdraw(&mut fixture, 1), PerpsError::WithdrawTooSoon);
}

/// Request and execute in the same slot is refused even with no delay
/// configured. This is what closes the atomic sandwich rather than merely
/// making it inconvenient.
#[test]
fn request_and_withdraw_in_one_slot_is_refused_even_with_no_delay() {
    let mut fixture = Fixture::new(InitializePoolParams {
        withdraw_delay_seconds: 0,
        ..default_params()
    });
    fixture.deposit(1_000 * ONE, 1).expect("deposit");

    let shares = fixture.token_balance(fixture.lp_share_account);
    request_withdraw(&mut fixture, shares).expect("request");

    // No clock advance at all: same slot.
    expect_error(lp_withdraw(&mut fixture, 1), PerpsError::WithdrawTooSoon);
}

/// A deposit that would mint fewer shares than the caller accepts is refused.
#[test]
fn slippage_protection_is_enforced() {
    let mut fixture = Fixture::new(default_params());
    expect_error(
        fixture.deposit(1_000 * ONE, u64::MAX),
        PerpsError::SlippageExceeded,
    );
}

/// A deposit past the pool's cap is refused. The cap is what bounds how much
/// can be lost while the protocol is unproven.
#[test]
fn the_deposit_cap_is_enforced() {
    let mut fixture = Fixture::new(InitializePoolParams {
        max_aum_quote: 100 * ONE,
        ..default_params()
    });
    expect_error(fixture.deposit(1_000 * ONE, 1), PerpsError::PoolCapReached);
}

/// A zero deposit is refused rather than minting zero shares silently.
#[test]
fn a_zero_deposit_is_refused() {
    let mut fixture = Fixture::new(default_params());
    expect_error(fixture.deposit(0, 0), PerpsError::ZeroAmount);
}

/// Only the admin may create the pool.
#[test]
fn a_non_admin_cannot_initialize_the_pool() {
    // Build a fixture whose pool is not yet created, by initialising a second
    // exchange is impossible — so assert on a fresh attempt with a stranger.
    let mut fixture = Fixture::new(default_params());
    let stranger = Keypair::new();
    fixture
        .svm
        .airdrop(&stranger.pubkey(), 10_000_000_000)
        .unwrap();

    let instruction = fixture.initialize_pool_ix(default_params(), stranger.pubkey());
    // The pool already exists, so this fails regardless; what matters is that
    // it is refused, and that a stranger is never the one who creates it.
    assert!(
        fixture.send(instruction, &[&stranger]).is_err(),
        "a stranger must not be able to initialise the pool"
    );
}

/// Presenting the wrong token program is refused. `Interface<TokenInterface>`
/// accepts both the legacy and Token-2022 programs, so the exchange pins which
/// one this collateral actually uses and this is the check that enforces it.
#[test]
fn the_wrong_token_program_is_refused() {
    let mut fixture = Fixture::new(default_params());

    let mut instruction = fixture.deposit_ix(1_000 * ONE, 1);
    // Swap legacy SPL Token for Token-2022.
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == spl_token_id() {
            meta.pubkey = token_2022_id();
        }
    }
    let lp = fixture.lp.insecure_clone();
    assert!(
        fixture.send(instruction, &[&lp]).is_err(),
        "a mismatched token program must be refused"
    );
}

/// Depositing from a token account somebody else owns is refused.
#[test]
fn depositing_from_another_owners_token_account_is_refused() {
    let mut fixture = Fixture::new(default_params());

    let stranger = Keypair::new();
    fixture
        .svm
        .airdrop(&stranger.pubkey(), 10_000_000_000)
        .unwrap();
    let stranger_account =
        CreateAssociatedTokenAccount::new(&mut fixture.svm, &stranger, &fixture.collateral_mint)
            .owner(&stranger.pubkey())
            .send()
            .expect("stranger token account");
    MintTo::new(
        &mut fixture.svm,
        &fixture.admin.insecure_clone(),
        &fixture.collateral_mint,
        &stranger_account,
        1_000 * ONE,
    )
    .send()
    .expect("fund stranger");

    // The LP signs, but points at the stranger's token account.
    let mut instruction = fixture.deposit_ix(1_000 * ONE, 1);
    for meta in instruction.accounts.iter_mut() {
        if meta.pubkey == fixture.lp_token_account {
            meta.pubkey = stranger_account;
        }
    }
    let lp = fixture.lp.insecure_clone();
    expect_error(fixture.send(instruction, &[&lp]), PerpsError::NotTokenOwner);
}

// ── helpers that need the fixture's accounts ────────────────────────────────

fn request_withdraw(fixture: &mut Fixture, shares: u64) -> Result<(), TransactionError> {
    let owner = fixture.lp.pubkey();
    let instruction = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::RequestWithdraw {
            owner,
            pool: fixture.pool,
            share_mint: fixture.share_mint,
            owner_share_account: fixture.lp_share_account,
            withdraw_request: pda(&[b"withdraw_request", owner.as_ref()]),
            escrow_share_account: pda(&[b"withdraw_escrow", owner.as_ref()]),
            token_program: spl_token_id(),
            system_program: anchor_lang::system_program::ID,
            rent: rent_sysvar_id(),
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::RequestWithdraw { shares }.data(),
    };
    let lp = fixture.lp.insecure_clone();
    fixture.send(instruction, &[&lp])
}

fn lp_withdraw(fixture: &mut Fixture, min_amount_out: u64) -> Result<(), TransactionError> {
    let owner = fixture.lp.pubkey();
    let instruction = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::LpWithdraw {
            owner,
            exchange: fixture.exchange,
            pool: fixture.pool,
            collateral_mint: fixture.collateral_mint,
            quote_vault: fixture.quote_vault,
            share_mint: fixture.share_mint,
            withdraw_request: pda(&[b"withdraw_request", owner.as_ref()]),
            escrow_share_account: pda(&[b"withdraw_escrow", owner.as_ref()]),
            owner_token_account: fixture.lp_token_account,
            token_program: spl_token_id(),
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::LpWithdraw { min_amount_out }.data(),
    };
    let lp = fixture.lp.insecure_clone();
    fixture.send(instruction, &[&lp])
}

fn close_stale_escrow(fixture: &mut Fixture) -> Result<(), TransactionError> {
    let owner = fixture.lp.pubkey();
    let instruction = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::CloseStaleEscrow {
            owner,
            pool: fixture.pool,
            withdraw_request: pda(&[b"withdraw_request", owner.as_ref()]),
            escrow_share_account: pda(&[b"withdraw_escrow", owner.as_ref()]),
            token_program: spl_token_id(),
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::CloseStaleEscrow {}.data(),
    };
    let lp = fixture.lp.insecure_clone();
    fixture.send(instruction, &[&lp])
}

fn cancel_withdraw(fixture: &mut Fixture) -> Result<(), TransactionError> {
    let owner = fixture.lp.pubkey();
    let instruction = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::CancelWithdraw {
            owner,
            pool: fixture.pool,
            share_mint: fixture.share_mint,
            owner_share_account: fixture.lp_share_account,
            withdraw_request: pda(&[b"withdraw_request", owner.as_ref()]),
            escrow_share_account: pda(&[b"withdraw_escrow", owner.as_ref()]),
            token_program: spl_token_id(),
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::CancelWithdraw {}.data(),
    };
    let lp = fixture.lp.insecure_clone();
    fixture.send(instruction, &[&lp])
}

/// A pending withdrawal can be abandoned, and every escrowed share comes back.
///
/// Without this, a request that cannot execute is a permanent trap: the
/// utilisation ceiling alone can keep `lp_withdraw` failing, and
/// `close_stale_escrow` refuses a funded escrow by design.
#[test]
fn a_pending_withdrawal_can_be_cancelled() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");

    let before = fixture.token_balance(fixture.lp_share_account);
    let shares = before / 2;
    request_withdraw(&mut fixture, shares).expect("request");
    assert_eq!(
        fixture.token_balance(fixture.lp_share_account),
        before - shares,
        "shares should be held in escrow"
    );

    cancel_withdraw(&mut fixture).expect("cancel");

    assert_eq!(
        fixture.token_balance(fixture.lp_share_account),
        before,
        "every escrowed share returns"
    );

    // Both accounts must be gone, or the next request fails with "already in
    // use" — the same orphan `close_stale_escrow` had to be written to undo.
    let escrow = pda(&[b"withdraw_escrow", fixture.lp.pubkey().as_ref()]);
    let request = pda(&[b"withdraw_request", fixture.lp.pubkey().as_ref()]);
    assert!(
        fixture
            .svm
            .get_account(&escrow)
            .is_none_or(|a| a.lamports == 0),
        "escrow must be closed"
    );
    assert!(
        fixture
            .svm
            .get_account(&request)
            .is_none_or(|a| a.lamports == 0),
        "request must be closed"
    );
}

/// Cancelling leaves the provider able to request again. A resource created
/// per-operation gets exercised twice — the lesson `lp_withdraw` taught.
#[test]
fn a_provider_can_request_again_after_cancelling() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");
    let shares = fixture.token_balance(fixture.lp_share_account);

    request_withdraw(&mut fixture, shares).expect("first request");
    cancel_withdraw(&mut fixture).expect("cancel");

    fixture.svm.expire_blockhash();
    request_withdraw(&mut fixture, shares).expect("second request after cancel");
}

/// The utilisation ceiling is bounded at construction by
/// `M5_MAX_UTILIZATION_BPS`, not merely below 100%.
///
/// This is the milestone's whole answer to LP share mispricing: the pool prices
/// shares off tracked equity with no mark, so the worst-case overstatement is
/// exactly `max_utilization_bps` of AUM. A pool created at 9 999 bps would carry
/// a ~100% bound instead of a 20% one — and nothing lowers the field on its own,
/// so it would carry it forever unless an admin happened to call
/// `set_pool_limits`. A ceiling one instruction enforces and another does not is
/// not a bound.
///
/// Asserted through a panic because `Fixture::new` expects the pool to
/// initialise; the panic *is* the rejection, and catching it keeps the test
/// exercising the real instruction rather than a re-derived predicate.
#[test]
fn a_utilization_ceiling_above_the_milestone_bound_is_refused() {
    for refused in [
        10_000,
        9_999,
        sakura_perps::M5_MAX_UTILIZATION_BPS + 1,
        // Zero makes every open impossible, which is what quarantining a market
        // is for; doing it pool-wide by parameter is an outage disguised as a
        // setting.
        0,
    ] {
        let mut params = default_params();
        params.max_utilization_bps = refused;

        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Fixture::new(params);
        }));
        assert!(
            built.is_err(),
            "initialize_pool must reject max_utilization_bps == {refused}"
        );
    }

    // The bound itself is accepted, so it is inclusive rather than the whole
    // range having been narrowed by one.
    let mut ok_params = default_params();
    ok_params.max_utilization_bps = sakura_perps::M5_MAX_UTILIZATION_BPS;
    Fixture::new(ok_params);
}

/// An escrow still holding shares is not stale, and must not be discarded.
///
/// This is the case that would burn a provider's claim: the shares are in
/// escrow against a live request, and the way out is `lp_withdraw`, not here.
#[test]
fn a_funded_escrow_is_not_closeable() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");
    let shares = fixture.token_balance(fixture.lp_share_account);
    request_withdraw(&mut fixture, shares).expect("request");

    expect_error(close_stale_escrow(&mut fixture), PerpsError::EscrowNotEmpty);
}

/// Nor is one whose request is still open, even after the shares are gone.
///
/// Reachable only in an inconsistent state, but the ordering matters: closing
/// the escrow out from under a live request would make that request permanently
/// unexecutable, which is the exact failure this instruction exists to undo.
#[test]
fn an_open_request_blocks_closing_its_escrow() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");
    let shares = fixture.token_balance(fixture.lp_share_account);
    request_withdraw(&mut fixture, shares).expect("request");

    // Empty the escrow without touching the request, so only the second guard
    // can be the one that fires.
    let escrow = pda(&[b"withdraw_escrow", fixture.lp.pubkey().as_ref()]);
    let mut account = fixture.svm.get_account(&escrow).expect("escrow exists");
    account.data[64..72].copy_from_slice(&0u64.to_le_bytes()); // SPL token amount
    fixture
        .svm
        .set_account(escrow, account)
        .expect("zero the escrow");

    expect_error(
        close_stale_escrow(&mut fixture),
        PerpsError::WithdrawRequestStillOpen,
    );
}

/// The case this exists for: an escrow left behind with no request, which the
/// pre-fix `lp_withdraw` produced on every completed withdrawal.
#[test]
fn an_orphaned_escrow_can_be_closed_by_its_owner() {
    let mut fixture = Fixture::new(default_params());
    fixture.deposit(1_000 * ONE, 1).expect("deposit");

    // Withdraw HALF, so shares remain to open a second request with. Taking
    // everything leaves nothing to escrow, and the orphan cannot be recreated.
    let half = fixture.token_balance(fixture.lp_share_account) / 2;
    request_withdraw(&mut fixture, half).expect("request");
    fixture.advance(61, 2);
    lp_withdraw(&mut fixture, 1).expect("withdraw");

    // lp_withdraw closes the escrow itself now, so the orphan has to be
    // recreated to test the recovery path: re-request, then delete the request
    // alone, leaving exactly the pre-fix shape.
    fixture.svm.expire_blockhash();
    let remaining = fixture.token_balance(fixture.lp_share_account);
    assert!(remaining > 0, "need shares left to recreate the escrow");
    request_withdraw(&mut fixture, remaining).expect("second request");

    let escrow = pda(&[b"withdraw_escrow", fixture.lp.pubkey().as_ref()]);
    let mut account = fixture.svm.get_account(&escrow).expect("escrow exists");
    account.data[64..72].copy_from_slice(&0u64.to_le_bytes());
    fixture
        .svm
        .set_account(escrow, account)
        .expect("zero the escrow");
    let request = pda(&[b"withdraw_request", fixture.lp.pubkey().as_ref()]);
    fixture
        .svm
        .set_account(request, solana_account::Account::default())
        .expect("orphan the escrow");

    let before = fixture
        .svm
        .get_account(&fixture.lp.pubkey())
        .unwrap()
        .lamports;
    close_stale_escrow(&mut fixture).expect("owner can reclaim an orphaned escrow");

    assert!(
        fixture
            .svm
            .get_account(&escrow)
            .is_none_or(|a| a.data.is_empty()),
        "the escrow should be gone",
    );
    let after = fixture
        .svm
        .get_account(&fixture.lp.pubkey())
        .unwrap()
        .lamports;
    assert!(after > before, "rent should be returned to the owner");

    // And the whole point: requesting a withdrawal is possible again. Deposit
    // afresh first — the orphaned request moved the previous shares into the
    // escrow that was then emptied by hand, so the LP holds none. Asserted
    // unconditionally: skipping this when there happen to be no shares is how
    // the first version of this test passed without proving anything.
    fixture.svm.expire_blockhash();
    fixture.deposit(100 * ONE, 1).expect("deposit again");
    let left = fixture.token_balance(fixture.lp_share_account);
    assert!(left > 0, "deposit should have minted shares");
    request_withdraw(&mut fixture, left).expect("request works again after recovery");
}
