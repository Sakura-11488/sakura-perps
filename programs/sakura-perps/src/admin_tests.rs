//! Host checks run the native entrypoint, generated constraints, and handlers. LiteSVM
//! additionally verifies runtime signatures, PDA ownership, and transaction
//! behavior against the compiled program.

use super::*;
use anchor_lang::InstructionData;

enum Action {
    Propose(Pubkey),
    Accept,
    Cancel,
}

fn info(
    key: Pubkey,
    owner: Pubkey,
    signer: bool,
    writable: bool,
    data: Vec<u8>,
) -> AccountInfo<'static> {
    AccountInfo::new(
        Box::leak(Box::new(key)),
        signer,
        writable,
        Box::leak(Box::new(1_000_000_000)),
        Box::leak(data.into_boxed_slice()),
        Box::leak(Box::new(owner)),
        false,
    )
}

fn fixture(admin: Pubkey) -> AccountInfo<'static> {
    let (address, bump) = Pubkey::find_program_address(&[b"exchange"], &ID);
    let mut data = Exchange::DISCRIMINATOR.to_vec();
    data.resize(8 + Exchange::INIT_SPACE, 0);
    let mut state = Exchange::try_deserialize(&mut data.as_slice()).unwrap();
    state.bump = bump;
    state.admin = admin;
    state.paused_flags = PauseFlags::ALL;
    state.fee_recipient = Pubkey::new_unique();
    state.keeper_fee_share_bps = 2_000;
    state._reserved = [42; 94];
    state.try_serialize(&mut data.as_mut_slice()).unwrap();
    info(address, ID, false, true, data)
}

fn state(exchange: &AccountInfo<'_>) -> Exchange {
    Exchange::try_deserialize(&mut exchange.data.borrow().as_ref()).unwrap()
}

fn run(
    exchange: &AccountInfo<'static>,
    actor: Pubkey,
    signed: bool,
    action: Action,
) -> std::result::Result<(), ProgramError> {
    let actor = info(
        actor,
        anchor_lang::system_program::ID,
        signed,
        false,
        vec![],
    );
    let ordered = match action {
        Action::Accept => vec![exchange.clone(), actor],
        _ => vec![actor, exchange.clone()],
    };
    let data = match action {
        Action::Propose(new_admin) => crate::instruction::ProposeAdmin { new_admin }.data(),
        Action::Accept => crate::instruction::AcceptAdmin {}.data(),
        Action::Cancel => crate::instruction::CancelAdminTransfer {}.data(),
    };
    crate::entry(
        &ID,
        Box::leak(ordered.into_boxed_slice()),
        Box::leak(data.into_boxed_slice()),
    )
}

fn refuses(
    exchange: &AccountInfo<'static>,
    actor: Pubkey,
    signed: bool,
    action: Action,
    code: u32,
) {
    let before = exchange.data.borrow().to_vec();
    assert_eq!(
        run(exchange, actor, signed, action),
        Err(ProgramError::Custom(code))
    );
    assert_eq!(exchange.data.borrow().as_ref(), before.as_slice());
}

fn code(error: PerpsError) -> u32 {
    error as u32 + anchor_lang::error::ERROR_CODE_OFFSET
}

#[test]
fn generated_constraints_enforce_the_rotation_state_machine() {
    let admin = Pubkey::new_unique();
    let first = Pubkey::new_unique();
    let second = Pubkey::new_unique();
    let stranger = Pubkey::new_unique();
    let exchange = fixture(admin);

    refuses(
        &exchange,
        first,
        true,
        Action::Accept,
        code(PerpsError::NoPendingAdmin),
    );
    refuses(
        &exchange,
        admin,
        true,
        Action::Cancel,
        code(PerpsError::NoPendingAdmin),
    );
    refuses(
        &exchange,
        stranger,
        true,
        Action::Propose(first),
        code(PerpsError::NotAdmin),
    );
    refuses(
        &exchange,
        admin,
        true,
        Action::Propose(Pubkey::default()),
        code(PerpsError::InvalidAdmin),
    );
    refuses(
        &exchange,
        admin,
        true,
        Action::Propose(admin),
        code(PerpsError::AdminUnchanged),
    );

    run(&exchange, admin, true, Action::Propose(first)).unwrap();
    assert_eq!(state(&exchange).admin, admin);
    assert_eq!(state(&exchange).pending_admin, first);
    refuses(
        &exchange,
        first,
        true,
        Action::Cancel,
        code(PerpsError::NotAdmin),
    );
    refuses(
        &exchange,
        stranger,
        true,
        Action::Accept,
        code(PerpsError::NotPendingAdmin),
    );
    run(&exchange, admin, true, Action::Propose(second)).unwrap();
    refuses(
        &exchange,
        first,
        true,
        Action::Accept,
        code(PerpsError::NotPendingAdmin),
    );
    run(&exchange, admin, true, Action::Cancel).unwrap();
    refuses(
        &exchange,
        second,
        true,
        Action::Accept,
        code(PerpsError::NoPendingAdmin),
    );

    let before = exchange.data.borrow().to_vec();
    run(&exchange, admin, true, Action::Propose(second)).unwrap();
    run(&exchange, second, true, Action::Accept).unwrap();
    assert_eq!(state(&exchange).admin, second);
    assert_eq!(state(&exchange).pending_admin, Pubkey::default());
    assert_eq!(state(&exchange).paused_flags, PauseFlags::ALL);
    assert_eq!(state(&exchange).keeper_fee_share_bps, 2_000);
    // Discriminator + bump occupy 9 bytes. Only admin and pending_admin follow.
    let after = exchange.data.borrow().to_vec();
    assert_eq!(&before[..9], &after[..9]);
    assert_eq!(&before[73..], &after[73..]);
    assert_eq!(after.len(), 312);
    refuses(
        &exchange,
        admin,
        true,
        Action::Propose(first),
        code(PerpsError::NotAdmin),
    );
    refuses(
        &exchange,
        second,
        true,
        Action::Accept,
        code(PerpsError::NoPendingAdmin),
    );
}

#[test]
fn generated_constraints_require_signatures_on_each_rotation_instruction() {
    let admin = Pubkey::new_unique();
    let nominee = Pubkey::new_unique();
    let exchange = fixture(admin);
    let error = anchor_lang::error::ErrorCode::AccountNotSigner as u32;
    refuses(&exchange, admin, false, Action::Propose(nominee), error);
    run(&exchange, admin, true, Action::Propose(nominee)).unwrap();
    refuses(&exchange, nominee, false, Action::Accept, error);
    refuses(&exchange, admin, false, Action::Cancel, error);
}
