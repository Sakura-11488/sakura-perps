//! Admin rotation exercises the actual Anchor constraints and deployed program
//! instructions, including unsigned account substitution and stale nominees.

use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, Space, ToAccountMetas};
use litesvm::LiteSVM;
use sakura_perps::{Exchange, PauseFlags, PerpsError};
use solana_account::Account;
use solana_address::Address;
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

struct Fixture {
    svm: LiteSVM,
    payer: Keypair,
    admin: Keypair,
    exchange: Address,
}

impl Fixture {
    fn new() -> Self {
        let path = std::env::var("SAKURA_PERPS_SO")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/deploy/sakura_perps.so")
            });
        let binary = std::fs::read(&path).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err}; build the program or set SAKURA_PERPS_SO",
                path.display()
            )
        });
        let mut svm = LiteSVM::new();
        svm.add_program(sakura_perps::ID, &binary)
            .expect("program loads");
        let payer = Keypair::new();
        let admin = Keypair::new();
        svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
        svm.airdrop(&admin.pubkey(), 1_000_000).unwrap();
        let (exchange, bump) = Address::find_program_address(&[b"exchange"], &sakura_perps::ID);

        // The pre-rotation layout, with nonzero unrelated fields and reserved
        // bytes so a transfer must preserve more than an all-zero fixture.
        let state = Exchange {
            bump,
            admin: admin.pubkey(),
            pending_admin: Address::default(),
            fee_recipient: Keypair::new().pubkey(),
            collateral_mint: Keypair::new().pubkey(),
            collateral_token_program: Keypair::new().pubkey(),
            collateral_decimals: 6,
            collateral_freeze_authority: Keypair::new().pubkey(),
            paused_flags: PauseFlags::ALL,
            protocol_fee_share_bps: 1_000,
            num_markets: 7,
            keeper_fee_share_bps: 2_000,
            _reserved: [42; 94],
        };
        let data = serialized(&state);
        assert_eq!(Exchange::INIT_SPACE, 304);
        assert_eq!(data.len(), 312);
        svm.set_account(
            exchange,
            Account {
                lamports: 1_000_000_000,
                data,
                owner: sakura_perps::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
        Self {
            svm,
            payer,
            admin,
            exchange,
        }
    }

    fn state(&self) -> Exchange {
        Exchange::try_deserialize(&mut self.bytes().as_slice()).unwrap()
    }

    fn bytes(&self) -> Vec<u8> {
        self.svm.get_account(&self.exchange).unwrap().data
    }

    fn signer(&mut self) -> Keypair {
        let signer = Keypair::new();
        self.svm.airdrop(&signer.pubkey(), 1_000_000).unwrap();
        signer
    }

    fn send(&mut self, ix: Instruction, signer: Option<&Keypair>) -> Result<(), TransactionError> {
        self.svm.expire_blockhash();
        let mut signers = vec![&self.payer];
        if let Some(signer) = signer {
            signers.push(signer);
        }
        let transaction = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &signers,
            self.svm.latest_blockhash(),
        );
        self.svm
            .send_transaction(transaction)
            .map(|_| ())
            .map_err(|error| error.err)
    }

    fn refuse(&mut self, ix: Instruction, signer: Option<&Keypair>, code: u32) {
        let before = self.bytes();
        assert_eq!(
            self.send(ix, signer),
            Err(TransactionError::InstructionError(
                0,
                InstructionError::Custom(code)
            )),
        );
        assert_eq!(
            self.bytes(),
            before,
            "a rejected call must change no exchange bytes"
        );
    }
}

fn serialized(exchange: &Exchange) -> Vec<u8> {
    let mut bytes = Vec::new();
    exchange.try_serialize(&mut bytes).unwrap();
    bytes
}

fn code(error: PerpsError) -> u32 {
    error as u32 + anchor_lang::error::ERROR_CODE_OFFSET
}

fn propose(exchange: Address, admin: Address, new_admin: Address) -> Instruction {
    Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::ProposeAdmin { admin, exchange }.to_account_metas(None),
        data: sakura_perps::instruction::ProposeAdmin { new_admin }.data(),
    }
}

fn accept(exchange: Address, pending_admin: Address) -> Instruction {
    Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::AcceptAdmin {
            exchange,
            pending_admin,
        }
        .to_account_metas(None),
        data: sakura_perps::instruction::AcceptAdmin {}.data(),
    }
}

fn cancel(exchange: Address, admin: Address) -> Instruction {
    Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::CancelAdminTransfer { admin, exchange }
            .to_account_metas(None),
        data: sakura_perps::instruction::CancelAdminTransfer {}.data(),
    }
}

fn pause(exchange: Address, admin: Address, flags: u64) -> Instruction {
    Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::SetPauseFlags { admin, exchange }.to_account_metas(None),
        data: sakura_perps::instruction::SetPauseFlags { flags }.data(),
    }
}

fn keeper_share(exchange: Address, admin: Address, bps: u16) -> Instruction {
    Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::SetKeeperFeeShare { admin, exchange }
            .to_account_metas(None),
        data: sakura_perps::instruction::SetKeeperFeeShare { bps }.data(),
    }
}

#[test]
fn acceptance_transfers_authority_while_paused_and_preserves_the_legacy_layout() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let nominee = fixture.signer();
    let exchange = fixture.exchange;
    let mut expected = fixture.state();
    fixture
        .send(
            propose(exchange, admin.pubkey(), nominee.pubkey()),
            Some(&admin),
        )
        .unwrap();
    expected.pending_admin = nominee.pubkey();
    assert_eq!(fixture.bytes(), serialized(&expected));

    // Nomination grants no power. The outgoing admin remains in control.
    fixture.refuse(
        pause(exchange, nominee.pubkey(), 0),
        Some(&nominee),
        code(PerpsError::NotAdmin),
    );
    fixture
        .send(
            pause(exchange, admin.pubkey(), PauseFlags::ALL),
            Some(&admin),
        )
        .unwrap();
    fixture
        .send(accept(exchange, nominee.pubkey()), Some(&nominee))
        .unwrap();
    expected.admin = nominee.pubkey();
    expected.pending_admin = Address::default();
    assert_eq!(
        fixture.bytes(),
        serialized(&expected),
        "only the two authority fields change"
    );

    fixture.refuse(
        pause(exchange, admin.pubkey(), 0),
        Some(&admin),
        code(PerpsError::NotAdmin),
    );
    fixture.refuse(
        keeper_share(exchange, admin.pubkey(), 1_000),
        Some(&admin),
        code(PerpsError::NotAdmin),
    );
    fixture.refuse(
        propose(exchange, admin.pubkey(), admin.pubkey()),
        Some(&admin),
        code(PerpsError::NotAdmin),
    );
    fixture.refuse(
        accept(exchange, nominee.pubkey()),
        Some(&nominee),
        code(PerpsError::NoPendingAdmin),
    );
    fixture
        .send(pause(exchange, nominee.pubkey(), 0), Some(&nominee))
        .unwrap();
    fixture
        .send(
            keeper_share(exchange, nominee.pubkey(), 1_000),
            Some(&nominee),
        )
        .unwrap();
    assert_eq!(fixture.state().paused_flags, 0);
    assert_eq!(fixture.state().keeper_fee_share_bps, 1_000);

    // The new admin can rotate again; the old admin cannot cancel its proposal.
    let successor = fixture.signer();
    fixture
        .send(
            propose(exchange, nominee.pubkey(), successor.pubkey()),
            Some(&nominee),
        )
        .unwrap();
    fixture.refuse(
        cancel(exchange, admin.pubkey()),
        Some(&admin),
        code(PerpsError::NotAdmin),
    );
    fixture
        .send(accept(exchange, successor.pubkey()), Some(&successor))
        .unwrap();
    assert_eq!(fixture.state().admin, successor.pubkey());
}

#[test]
fn only_the_current_admin_proposes_or_cancels_and_only_the_nominee_accepts() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let nominee = fixture.signer();
    let stranger = fixture.signer();
    let exchange = fixture.exchange;
    fixture.refuse(
        propose(exchange, stranger.pubkey(), nominee.pubkey()),
        Some(&stranger),
        code(PerpsError::NotAdmin),
    );
    fixture
        .send(
            propose(exchange, admin.pubkey(), nominee.pubkey()),
            Some(&admin),
        )
        .unwrap();
    for caller in [&admin, &stranger] {
        fixture.refuse(
            accept(exchange, caller.pubkey()),
            Some(caller),
            code(PerpsError::NotPendingAdmin),
        );
    }
    for caller in [&nominee, &stranger] {
        fixture.refuse(
            cancel(exchange, caller.pubkey()),
            Some(caller),
            code(PerpsError::NotAdmin),
        );
        fixture.refuse(
            propose(exchange, caller.pubkey(), stranger.pubkey()),
            Some(caller),
            code(PerpsError::NotAdmin),
        );
    }
}

#[test]
fn invalid_nominations_and_absent_transfers_fail_without_writes() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let nominee = fixture.signer();
    let exchange = fixture.exchange;
    fixture.refuse(
        accept(exchange, nominee.pubkey()),
        Some(&nominee),
        code(PerpsError::NoPendingAdmin),
    );
    fixture.refuse(
        cancel(exchange, admin.pubkey()),
        Some(&admin),
        code(PerpsError::NoPendingAdmin),
    );
    for pending in [false, true] {
        if pending {
            fixture
                .send(
                    propose(exchange, admin.pubkey(), nominee.pubkey()),
                    Some(&admin),
                )
                .unwrap();
        }
        fixture.refuse(
            propose(exchange, admin.pubkey(), Address::default()),
            Some(&admin),
            code(PerpsError::InvalidAdmin),
        );
        fixture.refuse(
            propose(exchange, admin.pubkey(), admin.pubkey()),
            Some(&admin),
            code(PerpsError::AdminUnchanged),
        );
    }
}

#[test]
fn cancellation_revokes_the_nominees_acceptance() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let nominee = fixture.signer();
    let exchange = fixture.exchange;
    let before = fixture.bytes();
    fixture
        .send(
            propose(exchange, admin.pubkey(), nominee.pubkey()),
            Some(&admin),
        )
        .unwrap();
    fixture
        .send(cancel(exchange, admin.pubkey()), Some(&admin))
        .unwrap();
    assert_eq!(fixture.bytes(), before);
    fixture.refuse(
        accept(exchange, nominee.pubkey()),
        Some(&nominee),
        code(PerpsError::NoPendingAdmin),
    );
    fixture.refuse(
        cancel(exchange, admin.pubkey()),
        Some(&admin),
        code(PerpsError::NoPendingAdmin),
    );
}

#[test]
fn replacement_revokes_the_previous_nominee() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let previous = fixture.signer();
    let current = fixture.signer();
    let exchange = fixture.exchange;
    fixture
        .send(
            propose(exchange, admin.pubkey(), previous.pubkey()),
            Some(&admin),
        )
        .unwrap();
    fixture
        .send(
            propose(exchange, admin.pubkey(), current.pubkey()),
            Some(&admin),
        )
        .unwrap();
    fixture.refuse(
        accept(exchange, previous.pubkey()),
        Some(&previous),
        code(PerpsError::NotPendingAdmin),
    );
    fixture
        .send(accept(exchange, current.pubkey()), Some(&current))
        .unwrap();
    assert_eq!(fixture.state().admin, current.pubkey());
}

#[test]
fn naming_the_authority_account_without_its_signature_is_insufficient() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let nominee = fixture.signer();
    let exchange = fixture.exchange;
    let unsigned = |mut ix: Instruction, authority: Address| {
        ix.accounts
            .iter_mut()
            .find(|account| account.pubkey == authority)
            .unwrap()
            .is_signer = false;
        ix
    };
    let error = anchor_lang::error::ErrorCode::AccountNotSigner as u32;
    fixture.refuse(
        unsigned(
            propose(exchange, admin.pubkey(), nominee.pubkey()),
            admin.pubkey(),
        ),
        None,
        error,
    );
    fixture
        .send(
            propose(exchange, admin.pubkey(), nominee.pubkey()),
            Some(&admin),
        )
        .unwrap();
    fixture.refuse(
        unsigned(accept(exchange, nominee.pubkey()), nominee.pubkey()),
        None,
        error,
    );
    fixture.refuse(
        unsigned(cancel(exchange, admin.pubkey()), admin.pubkey()),
        None,
        error,
    );
}

#[test]
fn a_forged_exchange_at_another_address_cannot_authorize_rotation() {
    let mut fixture = Fixture::new();
    let admin = fixture.admin.insecure_clone();
    let nominee = fixture.signer();
    let exchange = fixture.exchange;
    fixture
        .send(
            propose(exchange, admin.pubkey(), nominee.pubkey()),
            Some(&admin),
        )
        .unwrap();
    let forged = Keypair::new().pubkey();
    fixture
        .svm
        .set_account(forged, fixture.svm.get_account(&exchange).unwrap())
        .unwrap();
    let before = fixture.svm.get_account(&forged).unwrap().data;
    let error = anchor_lang::error::ErrorCode::ConstraintSeeds as u32;
    fixture.refuse(
        propose(forged, admin.pubkey(), nominee.pubkey()),
        Some(&admin),
        error,
    );
    fixture.refuse(accept(forged, nominee.pubkey()), Some(&nominee), error);
    fixture.refuse(cancel(forged, admin.pubkey()), Some(&admin), error);
    assert_eq!(fixture.svm.get_account(&forged).unwrap().data, before);
}

#[test]
fn rotation_preserves_existing_error_codes_and_exchange_size() {
    assert_eq!(Exchange::INIT_SPACE, 304);
    assert_eq!(code(PerpsError::PositionNotLiquidatable), 6062);
    assert_eq!(code(PerpsError::KeeperFeeShareTooHigh), 6067);
    assert_eq!(code(PerpsError::NotKeeperTokenOwner), 6068);
    assert_eq!(code(PerpsError::InvalidAdmin), 6069);
}
