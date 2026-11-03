//! Run: cargo run -p sakura-perps-rooms-auth --example signed_room --locked
//!
//! Public deterministic fixture keys only. Never fund these keys or reuse them
//! in a deployment. This example accesses no wallet, network, file or secret.
use std::collections::BTreeSet;

use ed25519_dalek::{Signer, SigningKey};
use sakura_perps_rooms::*;
use sakura_perps_rooms_auth::{
    wire::*, AuthOutcome, AuthenticatedRecord, AuthenticatedRoom, Error as AuthError,
    ExecutionContext,
};

const Q: u128 = 1_000_000;
const ADMIN: Actor = Actor::Owner(OwnerId(1));
const OWNER: Actor = Actor::Owner(OwnerId(2));
const BACKER: Actor = Actor::Owner(OwnerId(3));
const DELEGATE: Actor = Actor::Delegate(DelegateId(7));
const MEMBER: MemberId = MemberId(1);
const CELL: CellId = CellId(1);
const MARKET: MarketId = MarketId(1);
const ROOM: RoomId = RoomId(9);

fn fixture_key(actor: Actor) -> SigningKey {
    // Deliberately public, insecure fixture seeds; never operational credentials.
    let public_seed = match actor {
        ADMIN => 1,
        OWNER => 2,
        BACKER => 3,
        DELEGATE => 4,
        _ => panic!("actor is not part of this fixture"),
    };
    SigningKey::from_bytes(&[public_seed; 32])
}

fn genesis() -> Genesis {
    Genesis {
        domain: Domain {
            network: [11; 32],
            deployment: [22; 32],
            room: ROOM,
        },
        administrator: OwnerId(1),
        // Canonical order is Owner IDs followed by Delegate IDs.
        principals: [ADMIN, OWNER, BACKER, DELEGATE]
            .into_iter()
            .map(|actor| PrincipalBinding {
                actor,
                public_key: fixture_key(actor).verifying_key().to_bytes(),
            })
            .collect(),
    }
}

fn policy() -> Policy {
    Policy {
        contribution_limit_quote: 100 * Q,
        loss_limit_quote: 50 * Q,
        allowed_markets: BTreeSet::from([MARKET]),
        allowed_cells: BTreeSet::from([CELL]),
        max_leverage_bps: 20_000,
        max_position_notional_quote: 100 * Q,
        max_total_notional_quote: 100 * Q,
        max_open_positions: 2,
        max_fee_bps: 10,
        max_intent_lifetime_slots: 10,
    }
}

fn market(generation: u64) -> Market {
    Market {
        id: MARKET,
        generation,
        asset_decimals: 6,
        min_price: PRICE_SCALE / 10,
        max_price: 10 * PRICE_SCALE,
        max_oracle_age_slots: 2,
        max_position_lifetime_slots: 50,
        fee_bps: 10,
    }
}

fn quote(price: u128, slot: u64) -> Observation {
    Observation {
        market: MARKET,
        generation: 1,
        price,
        published_slot: slot,
    }
}

fn request(room: &AuthenticatedRoom, actor: Actor, action: Action, now: u64) -> SignedEnvelope {
    let authorization_revision = match &action {
        Action::Open { .. } | Action::Close { .. } => {
            Some(room.room().member(MEMBER).unwrap().policy_revision)
        }
        _ => None,
    };
    let intent = Intent {
        genesis_hash: room.genesis_hash(),
        actor,
        sequence: room.next_sequence(actor).unwrap(),
        not_before_slot: now,
        expires_slot: now + 10,
        authorization_revision,
        action,
    };
    let signature = fixture_key(actor)
        .sign(&signing_bytes(&intent).unwrap())
        .to_bytes();
    SignedEnvelope { intent, signature }
}

fn context(now: u64, observation: Option<Observation>) -> ExecutionContext {
    ExecutionContext {
        now_slot: now,
        observation,
    }
}

fn submit(
    room: &mut AuthenticatedRoom,
    actor: Actor,
    action: Action,
    now: u64,
    observation: Option<Observation>,
) -> AuthenticatedRecord {
    let envelope = request(room, actor, action, now);
    room.execute(&encode(&envelope).unwrap(), context(now, observation))
        .unwrap()
}

fn reject(
    room: &mut AuthenticatedRoom,
    envelope: &SignedEnvelope,
    context: ExecutionContext,
    expected: AuthError,
) {
    let before = room.clone();
    assert_eq!(
        room.execute(&encode(envelope).unwrap(), context),
        Err(expected)
    );
    assert_eq!(*room, before, "rejection changed state, nonce or history");
}

fn open(room: &AuthenticatedRoom, now: u64) -> Action {
    let member = room.room().member(MEMBER).unwrap();
    Action::Open {
        intent: OpenIntent {
            consent: Consent {
                room: ROOM,
                member: MEMBER,
                member_generation: member.generation,
                policy_revision: member.policy_revision,
                market_generation: 1,
                nonce: member.next_nonce,
                issued_slot: now,
                expires_slot: now + 10,
                min_execution_price: PRICE_SCALE,
                max_execution_price: 2 * PRICE_SCALE,
            },
            market: MARKET,
            cell: CELL,
            cell_policy_revision: room.room().cell(CELL).unwrap().policy_revision,
            side: Side::Long,
            size_base: 20 * Q,
            collateral_quote: 10 * Q,
            profit_cap_quote: 20 * Q,
            max_fee_quote: Q,
            maturity_slot: 30,
        },
    }
}

fn close(room: &AuthenticatedRoom, position: PositionId, now: u64) -> Action {
    // Economic revision stays pinned to admission; the outer signed request
    // separately binds the current authorization revision after revocation.
    let mut consent = room
        .room()
        .position(position)
        .unwrap()
        .intent
        .consent
        .clone();
    consent.nonce = room.room().member(MEMBER).unwrap().next_nonce;
    consent.issued_slot = now;
    consent.expires_slot = now + 10;
    Action::Close {
        intent: CloseIntent { consent, position },
    }
}

fn change_delegate(room: &mut AuthenticatedRoom, delegate: Option<DelegateId>, now: u64) {
    let expected_revision = room.room().member(MEMBER).unwrap().policy_revision;
    submit(
        room,
        OWNER,
        Action::ChangePolicy {
            member: MEMBER,
            expected_revision,
            delegate,
            policy: policy(),
        },
        now,
        None,
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let genesis = genesis();
    let mut room = AuthenticatedRoom::new(genesis.clone())?;
    for (actor, action) in [
        (ADMIN, Action::RegisterMarket { market: market(1) }),
        (
            BACKER,
            Action::AddCell {
                cell: CELL,
                policy: UnderwritingPolicy {
                    allowed_members: BTreeSet::from([(MEMBER, OwnerId(2), 1)]),
                    allowed_market_generations: BTreeSet::from([(MARKET, 1)]),
                    max_position_notional_quote: 100 * Q,
                    max_profit_cap_quote: 20 * Q,
                    max_leverage_bps: 20_000,
                    max_total_reserved_quote: 100 * Q,
                    min_fee_bps: 10,
                },
            },
        ),
        (
            BACKER,
            Action::FundCell {
                cell: CELL,
                amount_quote: 100 * Q,
            },
        ),
        (
            OWNER,
            Action::AddMember {
                member: MEMBER,
                delegate: Some(DelegateId(7)),
                policy: policy(),
            },
        ),
        (
            OWNER,
            Action::Contribute {
                member: MEMBER,
                amount_quote: 100 * Q,
            },
        ),
    ] {
        submit(&mut room, actor, action, 1, None);
    }

    let first_request = request(&room, DELEGATE, open(&room, 10), 10);
    let mut tampered = first_request.clone();
    tampered.intent.expires_slot += 1;
    reject(
        &mut room,
        &tampered,
        context(10, Some(quote(PRICE_SCALE, 10))),
        AuthError::InvalidSignature,
    );
    let first_record = room.execute(
        &encode(&first_request)?,
        context(10, Some(quote(PRICE_SCALE, 10))),
    )?;
    let AuthOutcome::Kernel(Outcome::Opened(first)) = first_record.outcome else {
        panic!("expected first position");
    };
    reject(
        &mut room,
        &first_request,
        context(10, Some(quote(PRICE_SCALE, 10))),
        AuthError::WrongSequence,
    );

    let held_close = request(&room, DELEGATE, close(&room, first.position, 11), 11);
    change_delegate(&mut room, None, 11);
    change_delegate(&mut room, Some(DelegateId(7)), 11);
    // Same delegate key and unused nonce do not resurrect a pre-revocation request.
    reject(
        &mut room,
        &held_close,
        context(12, Some(quote(PRICE_SCALE, 12))),
        AuthError::WrongAuthorizationRevision,
    );
    let owner_close = close(&room, first.position, 12);
    let owner_record = submit(
        &mut room,
        OWNER,
        owner_close,
        12,
        Some(quote(3 * PRICE_SCALE / 2, 12)),
    );
    let AuthOutcome::Kernel(Outcome::Closed(closed)) = owner_record.outcome else {
        panic!("expected owner close");
    };
    assert_eq!(closed.owner, OwnerId(2));
    assert_eq!(closed.payout_quote, first.payout_at(3 * PRICE_SCALE / 2)?);

    let second_open = open(&room, 13);
    let second_record = submit(
        &mut room,
        DELEGATE,
        second_open,
        13,
        Some(quote(PRICE_SCALE, 13)),
    );
    let AuthOutcome::Kernel(Outcome::Opened(second)) = second_record.outcome else {
        panic!("expected second position");
    };
    change_delegate(&mut room, None, 14);
    submit(
        &mut room,
        ADMIN,
        Action::RegisterMarket { market: market(2) },
        14,
        None,
    );
    // The administrator signs the original generation's terminal observation.
    // Its truth remains an external trust assumption, despite the valid signature.
    submit(
        &mut room,
        ADMIN,
        Action::RecordTerminalObservation {
            position: second.position,
            observation: quote(5 * PRICE_SCALE / 4, 30),
        },
        30,
        None,
    );
    let sequences_before = [ADMIN, OWNER, BACKER, DELEGATE].map(|actor| room.next_sequence(actor));
    let member_nonce = room.room().member(MEMBER).unwrap().next_nonce;
    let free_before = room.room().member(MEMBER).unwrap().free_quote;
    // This API accepts neither signer, price nor beneficiary; the admitted
    // position and immutable terminal observation determine the payout.
    let maturity_record = room.settle_matured(second.position, 30)?;
    let AuthOutcome::Kernel(Outcome::Closed(matured)) = maturity_record.outcome else {
        panic!("expected maturity settlement");
    };
    assert_eq!(matured.owner, OwnerId(2));
    assert_eq!(matured.member, MEMBER);
    assert_eq!(matured.payout_quote, second.payout_at(5 * PRICE_SCALE / 4)?);
    assert_eq!(
        room.room().member(MEMBER).unwrap().free_quote,
        free_before + matured.payout_quote
    );
    assert_eq!(room.room().member(MEMBER).unwrap().next_nonce, member_nonce);
    assert_eq!(
        [ADMIN, OWNER, BACKER, DELEGATE].map(|actor| room.next_sequence(actor)),
        sequences_before
    );
    let settled = room.clone();
    assert_eq!(
        room.settle_matured(second.position, 30),
        Err(AuthError::Kernel(Error::UnknownPosition))
    );
    assert_eq!(room, settled);

    let held_withdrawal = request(
        &room,
        OWNER,
        Action::Withdraw {
            member: MEMBER,
            amount_quote: Q,
        },
        31,
    );
    let cancellation = submit(&mut room, OWNER, Action::CancelPending, 31, None);
    assert!(matches!(
        cancellation.outcome,
        AuthOutcome::NonceAdvanced { actor: OWNER, .. }
    ));
    reject(
        &mut room,
        &held_withdrawal,
        context(31, None),
        AuthError::WrongSequence,
    );

    // Real consumers must obtain this head through an independent trusted
    // checkpoint. A transcript plus its own replacement head proves no history.
    let checkpoint = room.head_hash();
    assert_eq!(
        AuthenticatedRoom::replay(genesis.clone(), room.records(), checkpoint)?,
        room
    );
    let mut edited_records = room.records().to_vec();
    edited_records[0].hash[0] ^= 1;
    assert_eq!(
        AuthenticatedRoom::replay(genesis.clone(), &edited_records, checkpoint),
        Err(AuthError::TranscriptMismatch)
    );
    assert_eq!(
        AuthenticatedRoom::replay(
            genesis,
            &room.records()[..room.records().len() - 1],
            checkpoint
        ),
        Err(AuthError::TranscriptMismatch)
    );
    room.room().check_invariants()?;

    let signed = room
        .records()
        .iter()
        .filter(|record| {
            matches!(
                record.evidence,
                sakura_perps_rooms_auth::Evidence::Signed { .. }
            )
        })
        .count();
    let head = checkpoint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("Signed paper-room fixture: no custody, network or oracle-truth guarantee.");
    println!("Verified {signed} signed records + 1 permissionless settlement; head {head}");
    println!("Passed: tamper/replay rejection, revoke/regrant, owner close, fixed-beneficiary maturity, cancellation, transcript checks.");
    Ok(())
}
