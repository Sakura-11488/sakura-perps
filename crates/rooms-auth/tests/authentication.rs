use std::collections::BTreeSet;

use ed25519_dalek::{Signer, SigningKey};
use sakura_perps_rooms::*;
use sakura_perps_rooms_auth::{
    wire::*, AuthOutcome, AuthenticatedRoom, Error, ExecutionContext, MAX_AUTH_LIFETIME_SLOTS,
};

const Q: u128 = 1_000_000;
const ADMIN: Actor = Actor::Owner(OwnerId(1));
const OWNER: Actor = Actor::Owner(OwnerId(2));
const BACKER: Actor = Actor::Owner(OwnerId(3));
const STRANGER: Actor = Actor::Owner(OwnerId(9));
const DELEGATE: Actor = Actor::Delegate(DelegateId(7));

// Public deterministic test seeds, never a wallet or an operational credential.
fn key(actor: Actor) -> SigningKey {
    let seed = match actor {
        ADMIN => 1,
        OWNER => 2,
        BACKER => 3,
        STRANGER => 4,
        DELEGATE => 5,
        _ => 6,
    };
    SigningKey::from_bytes(&[seed; 32])
}

fn genesis() -> Genesis {
    Genesis {
        domain: Domain {
            network: [11; 32],
            deployment: [22; 32],
            room: RoomId(9),
        },
        administrator: OwnerId(1),
        principals: [ADMIN, OWNER, BACKER, STRANGER, DELEGATE]
            .into_iter()
            .map(|actor| PrincipalBinding {
                actor,
                public_key: key(actor).verifying_key().to_bytes(),
            })
            .collect(),
    }
}

fn policy() -> Policy {
    Policy {
        contribution_limit_quote: 100 * Q,
        loss_limit_quote: 50 * Q,
        allowed_markets: BTreeSet::from([MarketId(1)]),
        allowed_cells: BTreeSet::from([CellId(1)]),
        max_leverage_bps: 100_000,
        max_position_notional_quote: 200 * Q,
        max_total_notional_quote: 400 * Q,
        max_open_positions: 4,
        max_fee_bps: 10,
        max_intent_lifetime_slots: 20,
    }
}

fn backing() -> UnderwritingPolicy {
    UnderwritingPolicy {
        allowed_members: BTreeSet::from([(MemberId(1), OwnerId(2), 1)]),
        allowed_market_generations: BTreeSet::from([(MarketId(1), 1)]),
        max_position_notional_quote: 500 * Q,
        max_profit_cap_quote: 50 * Q,
        max_leverage_bps: 100_000,
        max_total_reserved_quote: 1_000 * Q,
        min_fee_bps: 1,
    }
}

fn quote(slot: u64) -> Observation {
    Observation {
        market: MarketId(1),
        generation: 1,
        price: 100 * PRICE_SCALE,
        published_slot: slot,
    }
}

fn request(room: &AuthenticatedRoom, actor: Actor, action: Action, now: u64) -> SignedEnvelope {
    let authorization_revision = match &action {
        Action::Open { intent } => room
            .room()
            .member(intent.consent.member)
            .map(|m| m.policy_revision),
        Action::Close { intent } => room
            .room()
            .member(intent.consent.member)
            .map(|m| m.policy_revision),
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
    let signature = key(actor).sign(&signing_bytes(&intent).unwrap()).to_bytes();
    SignedEnvelope { intent, signature }
}

fn submit(room: &mut AuthenticatedRoom, actor: Actor, action: Action, now: u64) {
    let observation =
        matches!(action, Action::Open { .. } | Action::Close { .. }).then(|| quote(now));
    let envelope = request(room, actor, action, now);
    room.execute(
        &encode(&envelope).unwrap(),
        ExecutionContext {
            now_slot: now,
            observation,
        },
    )
    .unwrap();
}

fn fixture() -> AuthenticatedRoom {
    let mut room = AuthenticatedRoom::new(genesis()).unwrap();
    submit(
        &mut room,
        ADMIN,
        Action::RegisterMarket {
            market: Market {
                id: MarketId(1),
                generation: 1,
                asset_decimals: 0,
                min_price: PRICE_SCALE,
                max_price: 200 * PRICE_SCALE,
                max_oracle_age_slots: 5,
                max_position_lifetime_slots: 20,
                fee_bps: 1,
            },
        },
        1,
    );
    submit(
        &mut room,
        BACKER,
        Action::AddCell {
            cell: CellId(1),
            policy: backing(),
        },
        1,
    );
    submit(
        &mut room,
        BACKER,
        Action::FundCell {
            cell: CellId(1),
            amount_quote: 1_000 * Q,
        },
        1,
    );
    submit(
        &mut room,
        OWNER,
        Action::AddMember {
            member: MemberId(1),
            delegate: Some(DelegateId(7)),
            policy: policy(),
        },
        1,
    );
    submit(
        &mut room,
        OWNER,
        Action::Contribute {
            member: MemberId(1),
            amount_quote: 100 * Q,
        },
        1,
    );
    room
}

fn open(room: &AuthenticatedRoom, now: u64) -> Action {
    let member = room.room().member(MemberId(1)).unwrap();
    Action::Open {
        intent: OpenIntent {
            consent: Consent {
                room: RoomId(9),
                member: MemberId(1),
                member_generation: 1,
                policy_revision: member.policy_revision,
                market_generation: 1,
                nonce: member.next_nonce,
                issued_slot: now,
                expires_slot: now + 10,
                min_execution_price: 99 * PRICE_SCALE,
                max_execution_price: 101 * PRICE_SCALE,
            },
            market: MarketId(1),
            cell: CellId(1),
            cell_policy_revision: 1,
            side: Side::Long,
            size_base: 1,
            collateral_quote: 10 * Q,
            profit_cap_quote: 20 * Q,
            max_fee_quote: Q,
            maturity_slot: now + 10,
        },
    }
}

fn close(room: &AuthenticatedRoom, now: u64) -> Action {
    let position = room.room().position(PositionId(1)).unwrap();
    let mut consent = position.intent.consent.clone();
    consent.nonce = room.room().member(MemberId(1)).unwrap().next_nonce;
    consent.issued_slot = now;
    consent.expires_slot = now + 10;
    Action::Close {
        intent: CloseIntent {
            consent,
            position: PositionId(1),
        },
    }
}

#[test]
fn signed_funding_and_withdrawal_are_single_use_and_owner_bound() {
    let mut room = fixture();
    let action = Action::Withdraw {
        member: MemberId(1),
        amount_quote: Q,
    };
    let envelope = request(&room, OWNER, action.clone(), 2);
    let record = room
        .execute(
            &encode(&envelope).unwrap(),
            ExecutionContext {
                now_slot: 2,
                observation: None,
            },
        )
        .unwrap();
    assert!(matches!(
        record.outcome,
        AuthOutcome::Kernel(Outcome::Withdrawn {
            owner: OwnerId(2),
            amount_quote: Q,
            ..
        })
    ));
    let before = room.clone();
    assert_eq!(
        room.execute(
            &encode(&envelope).unwrap(),
            ExecutionContext {
                now_slot: 2,
                observation: None
            }
        ),
        Err(Error::WrongSequence)
    );
    assert_eq!(room, before);
    let delegate_request = request(&room, DELEGATE, action, 2);
    assert_eq!(
        room.execute(
            &encode(&delegate_request).unwrap(),
            ExecutionContext {
                now_slot: 2,
                observation: None
            }
        ),
        Err(Error::Kernel(sakura_perps_rooms::Error::Unauthorized))
    );
    assert_eq!(room, before);
}

#[test]
fn every_byte_of_an_accepted_request_is_authenticated_or_rejected_by_the_codec() {
    let room = fixture();
    let envelope = request(
        &room,
        OWNER,
        Action::Withdraw {
            member: MemberId(1),
            amount_quote: Q,
        },
        2,
    );
    let bytes = encode(&envelope).unwrap();
    for index in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[index] ^= 1;
        let mut target = room.clone();
        assert!(
            target
                .execute(
                    &changed,
                    ExecutionContext {
                        now_slot: 2,
                        observation: None
                    }
                )
                .is_err(),
            "accepted byte mutation at {index}"
        );
        assert_eq!(target, room);
    }
}

#[test]
fn registry_rejects_ambiguous_roles_weak_keys_missing_admin_and_oversized_rosters() {
    let mut cases = Vec::new();
    let mut g = genesis();
    g.principals.swap(0, 1);
    cases.push(g);
    let mut g = genesis();
    g.principals[1].actor = ADMIN;
    cases.push(g);
    let mut g = genesis();
    g.principals[1].public_key = g.principals[0].public_key;
    cases.push(g);
    let mut g = genesis();
    g.principals[1].public_key = [0; 32];
    cases.push(g);
    let mut g = genesis();
    g.principals[1].public_key = [0; 32];
    g.principals[1].public_key[0] = 1;
    cases.push(g);
    let mut g = genesis();
    g.administrator = OwnerId(999);
    cases.push(g);
    let mut g = genesis();
    g.domain.network = [0; 32];
    cases.push(g);
    let mut g = genesis();
    g.principals.clear();
    cases.push(g);
    let mut g = genesis();
    g.principals = vec![g.principals[0].clone(); 129];
    cases.push(g);
    for g in cases {
        assert_eq!(AuthenticatedRoom::new(g), Err(Error::InvalidGenesis));
    }
}

#[test]
fn signatures_bind_network_deployment_room_and_entire_key_registry() {
    let room = fixture();
    let envelope = request(&room, OWNER, Action::CancelPending, 2);
    let mut variants = Vec::new();
    let mut g = genesis();
    g.domain.network[0] ^= 1;
    variants.push(g);
    let mut g = genesis();
    g.domain.deployment[0] ^= 1;
    variants.push(g);
    let mut g = genesis();
    g.domain.room.0 += 1;
    variants.push(g);
    let mut g = genesis();
    g.principals[1].public_key = key(Actor::Owner(OwnerId(999))).verifying_key().to_bytes();
    variants.push(g);
    for g in variants {
        let mut different = AuthenticatedRoom::new(g).unwrap();
        let before = different.clone();
        assert_eq!(
            different.execute(
                &encode(&envelope).unwrap(),
                ExecutionContext {
                    now_slot: 2,
                    observation: None
                }
            ),
            Err(Error::WrongDomain)
        );
        assert_eq!(different, before);
    }
}

#[test]
fn operator_clock_enforces_window_boundaries_and_monotonicity_for_all_actions() {
    let room = fixture();
    for (not_before, expires, now, valid) in [
        (2, 5, 1, false),
        (2, 5, 2, true),
        (2, 5, 5, true),
        (2, 5, 6, false),
        (5, 2, 3, false),
        (0, MAX_AUTH_LIFETIME_SLOTS + 1, 3, false),
    ] {
        let mut envelope = request(&room, OWNER, Action::CancelPending, 2);
        envelope.intent.not_before_slot = not_before;
        envelope.intent.expires_slot = expires;
        envelope.signature = key(OWNER)
            .sign(&signing_bytes(&envelope.intent).unwrap())
            .to_bytes();
        let mut target = room.clone();
        assert_eq!(
            target
                .execute(
                    &encode(&envelope).unwrap(),
                    ExecutionContext {
                        now_slot: now,
                        observation: None
                    }
                )
                .is_ok(),
            valid
        );
        if !valid {
            assert_eq!(target, room);
        }
    }
    let mut room = room;
    submit(&mut room, OWNER, Action::CancelPending, 5);
    let envelope = request(&room, BACKER, Action::CancelPending, 4);
    let before = room.clone();
    assert_eq!(
        room.execute(
            &encode(&envelope).unwrap(),
            ExecutionContext {
                now_slot: 4,
                observation: None
            }
        ),
        Err(Error::InvalidTime)
    );
    assert_eq!(room, before);
}

#[test]
fn cancel_consumes_only_its_signers_sequence_and_cancels_a_held_withdrawal() {
    let mut room = fixture();
    let held = request(
        &room,
        OWNER,
        Action::Withdraw {
            member: MemberId(1),
            amount_quote: Q,
        },
        2,
    );
    let before_kernel = room.room().clone();
    let delegate_sequence = room.next_sequence(DELEGATE);
    submit(&mut room, OWNER, Action::CancelPending, 2);
    assert_eq!(room.room(), &before_kernel);
    assert_eq!(room.next_sequence(DELEGATE), delegate_sequence);
    assert_eq!(
        room.execute(
            &encode(&held).unwrap(),
            ExecutionContext {
                now_slot: 2,
                observation: None
            }
        ),
        Err(Error::WrongSequence)
    );
}

#[test]
fn revoked_and_regranted_delegate_cannot_reuse_an_old_close_signature() {
    let mut room = fixture();
    let opening = open(&room, 2);
    submit(&mut room, DELEGATE, opening, 2);
    let held = request(&room, DELEGATE, close(&room, 3), 3);
    submit(
        &mut room,
        OWNER,
        Action::ChangePolicy {
            member: MemberId(1),
            expected_revision: 1,
            delegate: None,
            policy: policy(),
        },
        3,
    );
    submit(
        &mut room,
        OWNER,
        Action::ChangePolicy {
            member: MemberId(1),
            expected_revision: 2,
            delegate: Some(DelegateId(7)),
            policy: policy(),
        },
        3,
    );
    let before = room.clone();
    assert_eq!(
        room.execute(
            &encode(&held).unwrap(),
            ExecutionContext {
                now_slot: 3,
                observation: Some(quote(3))
            }
        ),
        Err(Error::WrongAuthorizationRevision)
    );
    assert_eq!(room, before);
    let closing = close(&room, 3);
    // Frozen economics keep revision1 while current authorization binds revision3.
    let current = request(&room, DELEGATE, closing, 3);
    assert_eq!(current.intent.authorization_revision, Some(3));
    let Action::Close { intent } = &current.intent.action else {
        panic!()
    };
    assert_eq!(intent.consent.policy_revision, 1);
    room.execute(
        &encode(&current).unwrap(),
        ExecutionContext {
            now_slot: 3,
            observation: Some(quote(3)),
        },
    )
    .unwrap();
    assert!(room.room().position(PositionId(1)).is_none());
}

#[test]
fn trade_prices_are_host_inputs_and_failed_execution_preserves_all_state() {
    let mut room = fixture();
    let envelope = request(&room, DELEGATE, open(&room, 2), 2);
    let bytes = encode(&envelope).unwrap();
    let before = room.clone();
    assert_eq!(
        room.execute(
            &bytes,
            ExecutionContext {
                now_slot: 2,
                observation: None
            }
        ),
        Err(Error::UnexpectedContext)
    );
    let mut invalid_quote = quote(2);
    invalid_quote.price = 150 * PRICE_SCALE;
    assert_eq!(
        room.execute(
            &bytes,
            ExecutionContext {
                now_slot: 2,
                observation: Some(invalid_quote)
            }
        ),
        Err(Error::Kernel(sakura_perps_rooms::Error::PriceOutOfBounds))
    );
    assert_eq!(room, before);
    room.execute(
        &bytes,
        ExecutionContext {
            now_slot: 2,
            observation: Some(quote(2)),
        },
    )
    .unwrap();
    let wrong_context = request(&room, OWNER, Action::CancelPending, 3);
    let before = room.clone();
    assert_eq!(
        room.execute(
            &encode(&wrong_context).unwrap(),
            ExecutionContext {
                now_slot: 3,
                observation: Some(quote(3))
            }
        ),
        Err(Error::UnexpectedContext)
    );
    assert_eq!(room, before);
}

#[test]
fn maturity_stays_permissionless_fixed_beneficiary_and_independent_of_trade_nonces() {
    let mut room = fixture();
    let opening = open(&room, 2);
    submit(&mut room, DELEGATE, opening, 2);
    submit(
        &mut room,
        OWNER,
        Action::ChangePolicy {
            member: MemberId(1),
            expected_revision: 1,
            delegate: None,
            policy: policy(),
        },
        3,
    );
    let before = room.clone();
    assert!(room.settle_matured(PositionId(1), 12).is_err());
    assert_eq!(room, before);
    submit(
        &mut room,
        ADMIN,
        Action::RecordTerminalObservation {
            position: PositionId(1),
            observation: quote(12),
        },
        12,
    );
    let nonces: Vec<_> = [ADMIN, OWNER, BACKER, DELEGATE]
        .iter()
        .map(|a| room.next_sequence(*a))
        .collect();
    let member_nonce = room.room().member(MemberId(1)).unwrap().next_nonce;
    let result = room.settle_matured(PositionId(1), 12).unwrap();
    assert!(matches!(
        result.outcome,
        AuthOutcome::Kernel(Outcome::Closed(CloseReceipt {
            owner: OwnerId(2),
            ..
        }))
    ));
    assert_eq!(
        member_nonce,
        room.room().member(MemberId(1)).unwrap().next_nonce
    );
    assert_eq!(
        nonces,
        [ADMIN, OWNER, BACKER, DELEGATE]
            .iter()
            .map(|a| room.next_sequence(*a))
            .collect::<Vec<_>>()
    );
    let before = room.clone();
    assert!(room.settle_matured(PositionId(1), 12).is_err());
    assert_eq!(room, before);
}

#[test]
fn replay_requires_signatures_exact_outputs_order_and_independent_head() {
    let mut room = fixture();
    submit(
        &mut room,
        OWNER,
        Action::Withdraw {
            member: MemberId(1),
            amount_quote: Q,
        },
        2,
    );
    assert_eq!(
        AuthenticatedRoom::replay(genesis(), room.records(), room.head_hash()).unwrap(),
        room
    );
    let mut records = room.records().to_vec();
    records[0].hash[0] ^= 1;
    assert!(AuthenticatedRoom::replay(genesis(), &records, room.head_hash()).is_err());
    let mut records = room.records().to_vec();
    records.swap(0, 1);
    assert!(AuthenticatedRoom::replay(genesis(), &records, room.head_hash()).is_err());
    let mut records = room.records().to_vec();
    records.pop();
    assert_eq!(
        AuthenticatedRoom::replay(genesis(), &records, room.head_hash()),
        Err(Error::TranscriptMismatch)
    );
    let mut records = room.records().to_vec();
    if let sakura_perps_rooms_auth::Evidence::Signed { envelope, .. } = &mut records[0].evidence {
        envelope.signature[0] ^= 1;
    }
    assert_eq!(
        AuthenticatedRoom::replay(genesis(), &records, room.head_hash()),
        Err(Error::InvalidSignature)
    );
}

#[test]
fn signed_intents_still_require_kernel_authority_and_resource_limits() {
    let mut room = fixture();
    let unauthorized = request(
        &room,
        STRANGER,
        Action::FundCell {
            cell: CellId(1),
            amount_quote: Q,
        },
        2,
    );
    let before = room.clone();
    assert_eq!(
        room.execute(
            &encode(&unauthorized).unwrap(),
            ExecutionContext {
                now_slot: 2,
                observation: None
            }
        ),
        Err(Error::Kernel(sakura_perps_rooms::Error::Unauthorized))
    );
    let mut p = policy();
    p.allowed_cells = (0..65).map(CellId).collect();
    let too_many = request(
        &room,
        OWNER,
        Action::ChangePolicy {
            member: MemberId(1),
            expected_revision: 1,
            delegate: None,
            policy: p,
        },
        2,
    );
    assert_eq!(
        room.execute(
            &encode(&too_many).unwrap(),
            ExecutionContext {
                now_slot: 2,
                observation: None
            }
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(room, before);
}

#[test]
fn valid_delegate_signatures_cannot_call_any_owner_or_administrator_operation() {
    let mut room = fixture();
    let opening = open(&room, 2);
    submit(&mut room, DELEGATE, opening, 2);
    let actions = [
        Action::RegisterMarket {
            market: Market {
                id: MarketId(1),
                generation: 2,
                asset_decimals: 0,
                min_price: PRICE_SCALE,
                max_price: 200 * PRICE_SCALE,
                max_oracle_age_slots: 5,
                max_position_lifetime_slots: 20,
                fee_bps: 1,
            },
        },
        Action::AddCell {
            cell: CellId(2),
            policy: backing(),
        },
        Action::ChangeCellPolicy {
            cell: CellId(1),
            expected_revision: 1,
            policy: backing(),
        },
        Action::AddMember {
            member: MemberId(2),
            delegate: None,
            policy: policy(),
        },
        Action::ChangePolicy {
            member: MemberId(1),
            expected_revision: 1,
            delegate: None,
            policy: policy(),
        },
        Action::Contribute {
            member: MemberId(1),
            amount_quote: Q,
        },
        Action::FundCell {
            cell: CellId(1),
            amount_quote: Q,
        },
        Action::Withdraw {
            member: MemberId(1),
            amount_quote: Q,
        },
        Action::WithdrawCell {
            cell: CellId(1),
            amount_quote: Q,
        },
        Action::RecordTerminalObservation {
            position: PositionId(1),
            observation: quote(12),
        },
    ];
    for action in actions {
        let before = room.clone();
        let envelope = request(&room, DELEGATE, action, 12);
        assert_eq!(
            room.execute(
                &encode(&envelope).unwrap(),
                ExecutionContext {
                    now_slot: 12,
                    observation: None
                }
            ),
            Err(Error::Kernel(sakura_perps_rooms::Error::Unauthorized))
        );
        assert_eq!(room, before);
    }
    for action in [open(&room, 3), close(&room, 3)] {
        let before = room.clone();
        let envelope = request(&room, STRANGER, action, 3);
        assert_eq!(
            room.execute(
                &encode(&envelope).unwrap(),
                ExecutionContext {
                    now_slot: 3,
                    observation: Some(quote(3))
                }
            ),
            Err(Error::Kernel(sakura_perps_rooms::Error::Unauthorized))
        );
        assert_eq!(room, before);
    }
}

#[test]
fn maximum_admitted_policy_sets_still_produce_a_complete_bounded_open_receipt() {
    let mut room = fixture();
    let mut p = policy();
    p.allowed_markets = (1..=64).map(MarketId).collect();
    p.allowed_cells = (1..=64).map(CellId).collect();
    submit(
        &mut room,
        OWNER,
        Action::ChangePolicy {
            member: MemberId(1),
            expected_revision: 1,
            delegate: Some(DelegateId(7)),
            policy: p,
        },
        2,
    );
    let mut p = backing();
    p.allowed_members = (1..=64).map(|id| (MemberId(id), OwnerId(2), 1)).collect();
    p.allowed_market_generations = (1..=64).map(|id| (MarketId(id), 1)).collect();
    submit(
        &mut room,
        BACKER,
        Action::ChangeCellPolicy {
            cell: CellId(1),
            expected_revision: 1,
            policy: p,
        },
        2,
    );
    let mut opening = open(&room, 3);
    if let Action::Open { intent } = &mut opening {
        intent.cell_policy_revision = 2;
    }
    submit(&mut room, DELEGATE, opening, 3);
    let record = room.records().last().unwrap();
    let bytes = encode(record).unwrap();
    assert!(bytes.len() < MAX_WIRE_BYTES);
    assert_eq!(
        decode::<sakura_perps_rooms_auth::AuthenticatedRecord>(&bytes).unwrap(),
        *record
    );
    assert_eq!(
        AuthenticatedRoom::replay(genesis(), room.records(), room.head_hash()).unwrap(),
        room
    );
}
