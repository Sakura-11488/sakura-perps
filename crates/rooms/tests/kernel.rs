use sakura_perps_rooms::*;
use std::collections::BTreeSet;

const Q: u128 = 1_000_000;
const D: DelegateId = DelegateId(99);
type IntentOracleMutation = (fn(&mut OpenIntent, &mut Observation), Error);
type IntentMutation = (fn(&mut OpenIntent), Error);

fn policy() -> Policy {
    Policy {
        contribution_limit_quote: 100 * Q,
        loss_limit_quote: 100 * Q,
        allowed_markets: BTreeSet::from([MarketId(1)]),
        allowed_cells: BTreeSet::from([CellId(1), CellId(2)]),
        max_leverage_bps: 50_000,
        max_position_notional_quote: 500 * Q,
        max_total_notional_quote: 1_000 * Q,
        max_open_positions: 4,
        max_fee_bps: 20,
        max_intent_lifetime_slots: 20,
    }
}

fn market() -> Market {
    Market {
        id: MarketId(1),
        generation: 1,
        asset_decimals: 6,
        min_price: PRICE_SCALE / 10,
        max_price: 10 * PRICE_SCALE,
        max_oracle_age_slots: 2,
        max_position_lifetime_slots: 100,
        fee_bps: 10,
    }
}

fn underwriting() -> UnderwritingPolicy {
    UnderwritingPolicy {
        allowed_members: BTreeSet::from([
            (MemberId(1), OwnerId(11), 1),
            (MemberId(2), OwnerId(12), 1),
        ]),
        allowed_market_generations: BTreeSet::from([(MarketId(1), 1)]),
        max_position_notional_quote: 500 * Q,
        max_profit_cap_quote: 300 * Q,
        max_leverage_bps: 50_000,
        max_total_reserved_quote: 1_000 * Q,
        min_fee_bps: 0,
    }
}

fn fixture() -> Room {
    let mut room = Room::new(RoomId(1), OwnerId(1));
    room.execute(Command::RegisterMarket {
        actor: Actor::Owner(OwnerId(1)),
        market: market(),
    })
    .unwrap();
    for i in 1..=2 {
        room.execute(Command::AddCell {
            actor: Actor::Owner(OwnerId(100 + i)),
            cell: CellId(i),
            policy: underwriting(),
        })
        .unwrap();
        room.execute(Command::FundCell {
            actor: Actor::Owner(OwnerId(100 + i)),
            cell: CellId(i),
            amount_quote: 200 * Q,
        })
        .unwrap();
        room.execute(Command::AddMember {
            actor: Actor::Owner(OwnerId(10 + i)),
            member: MemberId(i),
            delegate: Some(D),
            policy: policy(),
        })
        .unwrap();
        room.execute(Command::Contribute {
            actor: Actor::Owner(OwnerId(10 + i)),
            member: MemberId(i),
            amount_quote: 100 * Q,
        })
        .unwrap();
    }
    room
}

fn quote(price: u128, slot: u64) -> Observation {
    Observation {
        market: MarketId(1),
        generation: 1,
        price,
        published_slot: slot,
    }
}

fn consent(room: &Room, id: MemberId, now: u64) -> Consent {
    let m = room.member(id).unwrap();
    Consent {
        room: RoomId(1),
        member: id,
        member_generation: m.generation,
        policy_revision: m.policy_revision,
        market_generation: 1,
        nonce: m.next_nonce,
        issued_slot: now,
        expires_slot: now + 10,
        min_execution_price: PRICE_SCALE / 10,
        max_execution_price: 10 * PRICE_SCALE,
    }
}

fn open_intent(room: &Room, id: MemberId) -> OpenIntent {
    OpenIntent {
        consent: consent(room, id, room.current_slot().max(10)),
        market: MarketId(1),
        cell: CellId(1),
        cell_policy_revision: room.cell(CellId(1)).unwrap().policy_revision,
        side: Side::Long,
        size_base: 100 * Q,
        collateral_quote: 25 * Q,
        profit_cap_quote: 50 * Q,
        max_fee_quote: Q,
        maturity_slot: 100,
    }
}

fn open_command(room: &Room, id: MemberId) -> Command {
    let now = room.current_slot().max(10);
    Command::Open {
        actor: Actor::Delegate(D),
        intent: open_intent(room, id),
        observation: quote(PRICE_SCALE, now),
        now_slot: now,
    }
}

fn opened(room: &mut Room, command: Command) -> OpenReceipt {
    match room.execute(command).unwrap() {
        Outcome::Opened(r) => *r,
        other => panic!("{other:?}"),
    }
}

fn close_command(room: &Room, p: &OpenReceipt, price: u128, now: u64) -> Command {
    let mut c = consent(room, p.intent.consent.member, now);
    c.policy_revision = p.intent.consent.policy_revision;
    c.market_generation = p.market.generation;
    Command::Close {
        actor: Actor::Delegate(D),
        intent: CloseIntent {
            consent: c,
            position: p.position,
        },
        observation: Observation {
            generation: p.market.generation,
            ..quote(price, now)
        },
        now_slot: now,
    }
}

fn rejects(room: &mut Room, command: Command, error: Error) {
    let before = room.clone();
    let bytes = room.inspection_bytes();
    assert_eq!(room.execute(command), Err(error));
    assert_eq!(*room, before);
    assert_eq!(room.inspection_bytes(), bytes);
    room.check_invariants().unwrap();
}

#[test]
fn fully_funded_receipt_replays_exact_quote_payout_and_bound_owner() {
    let mut room = fixture();
    let before_cell = room.cell(CellId(1)).unwrap().clone();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    assert_eq!(p.entry_notional_quote, 100 * Q);
    assert_eq!(p.fee_quote, Q / 10);
    assert_eq!(p.maximum_payout_quote, 75 * Q);
    assert_eq!(
        room.cell(CellId(1)).unwrap().reserved_quote,
        p.maximum_payout_quote
    );
    assert_eq!(
        room.cell(CellId(1)).unwrap().balance_quote,
        before_cell.balance_quote + 25 * Q + p.fee_quote
    );
    let command = close_command(&room, &p, 2 * PRICE_SCALE, 11);
    let Outcome::Closed(close) = room.execute(command).unwrap() else {
        panic!()
    };
    assert_eq!(close.payout_quote, p.payout_at(2 * PRICE_SCALE).unwrap());
    assert_eq!(close.payout_quote, 75 * Q);
    assert_eq!(close.owner, OwnerId(11));
    let free = room.member(MemberId(1)).unwrap().free_quote;
    assert_eq!(
        room.execute(Command::Withdraw {
            actor: Actor::Owner(OwnerId(11)),
            member: MemberId(1),
            amount_quote: free
        })
        .unwrap(),
        Outcome::Withdrawn {
            member: MemberId(1),
            owner: OwnerId(11),
            amount_quote: free
        }
    );
    assert_eq!(
        Room::replay(RoomId(1), OwnerId(1), room.records()).unwrap(),
        room
    );
    let mut forged = room.records().to_vec();
    let record = forged
        .iter_mut()
        .find(|r| matches!(r.outcome, Outcome::Opened(_)))
        .unwrap();
    if let Outcome::Opened(p) = &mut record.outcome {
        p.fee_quote += 1;
    }
    assert_eq!(
        Room::replay(RoomId(1), OwnerId(1), &forged),
        Err(Error::ReplayMismatch)
    );
}

#[test]
fn delegate_cannot_move_external_funds_or_rewrite_consent() {
    let mut room = fixture();
    for command in [
        Command::Withdraw {
            actor: Actor::Delegate(D),
            member: MemberId(1),
            amount_quote: 1,
        },
        Command::Contribute {
            actor: Actor::Delegate(D),
            member: MemberId(1),
            amount_quote: 1,
        },
        Command::FundCell {
            actor: Actor::Delegate(D),
            cell: CellId(1),
            amount_quote: 1,
        },
        Command::WithdrawCell {
            actor: Actor::Delegate(D),
            cell: CellId(1),
            amount_quote: 1,
        },
        Command::ChangePolicy {
            actor: Actor::Delegate(D),
            member: MemberId(1),
            expected_revision: 1,
            delegate: Some(D),
            policy: policy(),
        },
        Command::Withdraw {
            actor: Actor::Owner(OwnerId(12)),
            member: MemberId(1),
            amount_quote: 1,
        },
    ] {
        rejects(&mut room, command, Error::Unauthorized);
    }
}

#[test]
fn consent_and_oracle_failures_leave_every_field_and_nonce_unchanged() {
    let mut room = fixture();
    let base = open_command(&room, MemberId(1));
    let mutations: Vec<IntentOracleMutation> = vec![
        (|i, _| i.consent.room = RoomId(2), Error::WrongRoom),
        (
            |i, _| i.consent.member_generation = 2,
            Error::WrongGeneration,
        ),
        (
            |i, _| i.consent.market_generation = 2,
            Error::WrongGeneration,
        ),
        (|i, _| i.consent.policy_revision = 2, Error::WrongPolicy),
        (|i, _| i.consent.nonce = 1, Error::WrongNonce),
        (
            |i, _| {
                i.consent.issued_slot = 0;
                i.consent.expires_slot = 9;
            },
            Error::ExpiredIntent,
        ),
        (|i, _| i.consent.issued_slot = 11, Error::InvalidTime),
        (|i, _| i.consent.expires_slot = 40, Error::InvalidTime),
        (
            |i, _| i.consent.min_execution_price = 2 * PRICE_SCALE,
            Error::PriceOutOfBounds,
        ),
        (
            |i, _| i.consent.min_execution_price = 0,
            Error::PriceOutOfBounds,
        ),
        (|_, o| o.generation = 2, Error::WrongGeneration),
        (|_, o| o.published_slot = 7, Error::StaleOracle),
        (|_, o| o.published_slot = 11, Error::FutureOracle),
        (|_, o| o.price = 0, Error::PriceOutOfBounds),
        (|i, _| i.size_base = 0, Error::InvalidAmount),
        (|i, _| i.maturity_slot = 10, Error::InvalidTime),
        (|i, _| i.maturity_slot = 111, Error::InvalidTime),
    ];
    for (mutate, error) in mutations {
        let mut command = base.clone();
        if let Command::Open {
            intent,
            observation,
            ..
        } = &mut command
        {
            mutate(intent, observation);
        }
        rejects(&mut room, command, error);
    }
    opened(&mut room, base.clone());
    rejects(&mut room, base, Error::WrongNonce);
}

#[test]
fn external_contribution_budget_never_resets_after_withdrawal_or_profit() {
    let mut room = fixture();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    room.execute(close_command(&room, &p, 2 * PRICE_SCALE, 11))
        .unwrap();
    let free = room.member(MemberId(1)).unwrap().free_quote;
    assert!(free > 100 * Q);
    room.execute(Command::Withdraw {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        amount_quote: free,
    })
    .unwrap();
    rejects(
        &mut room,
        Command::Contribute {
            actor: Actor::Owner(OwnerId(11)),
            member: MemberId(1),
            amount_quote: 1,
        },
        Error::ContributionBudget,
    );
    let before_used = room.member(MemberId(1)).unwrap().loss_and_fee_used_quote;
    let mut expanded = policy();
    expanded.contribution_limit_quote = 150 * Q;
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        expected_revision: 1,
        delegate: Some(D),
        policy: expanded,
    })
    .unwrap();
    assert_eq!(room.member(MemberId(1)).unwrap().contributed_quote, 100 * Q);
    assert_eq!(
        room.member(MemberId(1)).unwrap().loss_and_fee_used_quote,
        before_used
    );
    room.execute(Command::Contribute {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        amount_quote: 50 * Q,
    })
    .unwrap();
    rejects(
        &mut room,
        Command::Contribute {
            actor: Actor::Owner(OwnerId(11)),
            member: MemberId(1),
            amount_quote: 1,
        },
        Error::ContributionBudget,
    );
}

#[test]
fn simultaneous_opens_reserve_the_unspent_lifetime_loss_budget() {
    let mut room = fixture();
    let mut limited = policy();
    limited.loss_limit_quote = 50 * Q;
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        expected_revision: 1,
        delegate: Some(D),
        policy: limited,
    })
    .unwrap();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    assert_eq!(
        room.member(MemberId(1)).unwrap().collateral_at_risk_quote,
        25 * Q
    );
    let second = open_command(&room, MemberId(1));
    // 25.1 already encumbered + another 25.1 > the lifetime 50 budget.
    rejects(&mut room, second, Error::LossBudget);
    room.execute(close_command(&room, &p, PRICE_SCALE / 10, 11))
        .unwrap();
    assert_eq!(
        room.member(MemberId(1)).unwrap().loss_and_fee_used_quote,
        25 * Q + Q / 10
    );
    assert_eq!(
        room.member(MemberId(1)).unwrap().collateral_at_risk_quote,
        0
    );
    let again = open_command(&room, MemberId(1));
    rejects(&mut room, again, Error::LossBudget);
    // A policy revision cannot erase already used budget either.
    let mut same = policy();
    same.loss_limit_quote = 50 * Q;
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        expected_revision: 2,
        delegate: Some(D),
        policy: same,
    })
    .unwrap();
    let again = open_command(&room, MemberId(1));
    rejects(&mut room, again, Error::LossBudget);
}

#[test]
fn funding_and_losses_cannot_cross_cells_or_members() {
    let mut room = fixture();
    let untouched_cell = room.cell(CellId(2)).unwrap().clone();
    let untouched_member = room.member(MemberId(2)).unwrap().clone();
    let mut too_large = open_command(&room, MemberId(1));
    if let Command::Open { intent, .. } = &mut too_large {
        intent.profit_cap_quote = 201 * Q;
    }
    rejects(&mut room, too_large, Error::InsufficientBacking);
    let p = opened(&mut room, open_command(&fixture(), MemberId(1)));
    let mut impostor_close = close_command(&room, &p, 2 * PRICE_SCALE, 11);
    if let Command::Close { intent, .. } = &mut impostor_close {
        intent.consent = consent(&room, MemberId(2), 11);
    }
    rejects(&mut room, impostor_close, Error::Unauthorized);
    room.execute(close_command(&room, &p, 10 * PRICE_SCALE, 11))
        .unwrap();
    assert_eq!(room.cell(CellId(2)).unwrap(), &untouched_cell);
    assert_eq!(room.member(MemberId(2)).unwrap(), &untouched_member);
    assert_eq!(
        room.cell(CellId(1)).unwrap().balance_quote,
        150 * Q + Q / 10
    );
}

#[test]
fn owner_can_withdraw_only_the_cells_unreserved_balance() {
    let mut room = fixture();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    let free = room.cell(CellId(1)).unwrap().free_quote().unwrap();
    rejects(
        &mut room,
        Command::WithdrawCell {
            actor: Actor::Owner(OwnerId(101)),
            cell: CellId(1),
            amount_quote: free + 1,
        },
        Error::InsufficientBacking,
    );
    room.execute(Command::WithdrawCell {
        actor: Actor::Owner(OwnerId(101)),
        cell: CellId(1),
        amount_quote: free,
    })
    .unwrap();
    room.execute(close_command(&room, &p, 2 * PRICE_SCALE, 11))
        .unwrap();
    assert_eq!(room.cell(CellId(1)).unwrap().balance_quote, 0);
    assert_eq!(room.cell(CellId(1)).unwrap().reserved_quote, 0);
}

#[test]
fn policy_and_generation_changes_do_not_reprice_existing_contracts() {
    let mut room = fixture();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    let mut next_market = market();
    next_market.generation = 2;
    next_market.fee_bps = 1_000;
    room.execute(Command::RegisterMarket {
        actor: Actor::Owner(OwnerId(1)),
        market: next_market,
    })
    .unwrap();
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        expected_revision: 1,
        delegate: None,
        policy: policy(),
    })
    .unwrap();
    assert_eq!(room.position(p.position), Some(&p));
    let command = close_command(&room, &p, 2 * PRICE_SCALE, 11);
    rejects(&mut room, command.clone(), Error::Unauthorized);
    let mut owner_command = command;
    if let Command::Close { actor, .. } = &mut owner_command {
        *actor = Actor::Owner(OwnerId(11));
    }
    let Outcome::Closed(c) = room.execute(owner_command).unwrap() else {
        panic!()
    };
    assert_eq!(c.payout_quote, 75 * Q);
    assert_eq!(c.policy_revision_at_open, 1);
    assert_eq!(c.market_generation, 1);
}

#[test]
fn finite_contract_requires_exact_maturity_observation_and_allows_late_replay() {
    let mut room = fixture();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    let command = close_command(&room, &p, 2 * PRICE_SCALE, 1_000);
    rejects(
        &mut room,
        command.clone(),
        Error::MissingMaturityObservation,
    );
    let mut terminal = command;
    if let Command::Close { observation, .. } = &mut terminal {
        observation.published_slot = 100;
    }
    room.execute(Command::RecordTerminalObservation {
        actor: Actor::Owner(OwnerId(1)),
        position: p.position,
        observation: quote(2 * PRICE_SCALE, 100),
        now_slot: 1_000,
    })
    .unwrap();
    let Outcome::Closed(c) = room.execute(terminal.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(c.observation.published_slot, 100);
    assert_eq!(c.payout_quote, 75 * Q);
    rejects(&mut room, terminal, Error::WrongNonce);
}

#[test]
fn payout_is_monotone_capped_and_nonnegative_on_an_exhaustive_price_grid() {
    for side in [Side::Long, Side::Short] {
        let mut room = fixture();
        let mut command = open_command(&room, MemberId(1));
        if let Command::Open { intent, .. } = &mut command {
            intent.side = side;
        }
        let p = opened(&mut room, command);
        let mut last = p.payout_at(p.market.min_price).unwrap();
        for step in 10..=1_000 {
            let payout = p.payout_at(PRICE_SCALE * step / 100).unwrap();
            assert!(payout <= p.maximum_payout_quote);
            match side {
                Side::Long => assert!(payout >= last),
                Side::Short => assert!(payout <= last),
            }
            last = payout;
        }
    }
}

fn permutations(values: Vec<usize>) -> Vec<Vec<usize>> {
    if values.is_empty() {
        return vec![vec![]];
    }
    let mut result = vec![];
    for i in 0..values.len() {
        let mut rest = values.clone();
        let head = rest.remove(i);
        for mut tail in permutations(rest) {
            tail.insert(0, head);
            result.push(tail);
        }
    }
    result
}

#[test]
fn fixed_price_payouts_are_independent_of_all_four_position_close_orders() {
    let mut room = fixture();
    let mut positions = vec![];
    for i in 0..4 {
        let mut command = open_command(&room, MemberId(1 + i % 2));
        if let Command::Open { intent, .. } = &mut command {
            intent.side = if i < 2 { Side::Long } else { Side::Short };
        }
        positions.push(opened(&mut room, command));
    }
    let mut reference = None;
    for order in permutations(vec![0, 1, 2, 3]) {
        let mut run = room.clone();
        for i in order {
            run.execute(close_command(&run, &positions[i], 2 * PRICE_SCALE, 11))
                .unwrap();
        }
        let state = (
            run.member(MemberId(1)).unwrap().clone(),
            run.member(MemberId(2)).unwrap().clone(),
            run.cell(CellId(1)).unwrap().clone(),
        );
        if let Some(expected) = &reference {
            assert_eq!(&state, expected);
        } else {
            reference = Some(state);
        }
        run.check_invariants().unwrap();
    }
}

#[test]
fn limits_bind_actual_debit_and_do_not_accept_floor_leverage() {
    let mutations: Vec<IntentMutation> = vec![
        (|i| i.collateral_quote = 20 * Q - 1, Error::LeverageLimit),
        (|i| i.size_base = 501 * Q, Error::NotionalLimit),
        (|i| i.max_fee_quote = Q / 10 - 1, Error::FeeLimit),
        (|i| i.collateral_quote = 100 * Q, Error::InsufficientBalance),
        (|i| i.market = MarketId(9), Error::MarketNotAllowed),
        (|i| i.cell = CellId(9), Error::CellNotAllowed),
        (|i| i.size_base = u128::MAX, Error::NotionalLimit),
    ];
    for (mutate, error) in mutations {
        let mut room = fixture();
        let mut command = open_command(&room, MemberId(1));
        if let Command::Open { intent, .. } = &mut command {
            mutate(intent);
        }
        rejects(&mut room, command, error);
    }
    let mut room = fixture();
    let mut limited = policy();
    limited.max_open_positions = 1;
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        expected_revision: 1,
        delegate: Some(D),
        policy: limited,
    })
    .unwrap();
    let first = open_command(&room, MemberId(1));
    opened(&mut room, first);
    let second = open_command(&room, MemberId(1));
    rejects(&mut room, second, Error::PositionLimit);
}

#[test]
fn late_arithmetic_failure_rolls_back_preceding_cell_mutations() {
    let mut room = fixture();
    room.execute(Command::AddCell {
        actor: Actor::Owner(OwnerId(103)),
        cell: CellId(3),
        policy: underwriting(),
    })
    .unwrap();
    // Empty cell accepts MAX locally, then global external-in overflows. The
    // clone/commit boundary must restore the already-changed cell exactly.
    rejects(
        &mut room,
        Command::FundCell {
            actor: Actor::Owner(OwnerId(103)),
            cell: CellId(3),
            amount_quote: u128::MAX,
        },
        Error::Arithmetic,
    );
}

#[test]
fn deterministic_state_machine_checks_conservation_atomicity_and_transcript_replay() {
    // Finite generated campaign, not a proof or economic market simulation.
    for seed in 1..=32u64 {
        let mut room = fixture();
        let mut rng = seed;
        for step in 0..64 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let id = MemberId(1 + rng % 2);
            let command = match rng % 6 {
                0 => Command::Withdraw {
                    actor: Actor::Owner(OwnerId(10 + id.0)),
                    member: id,
                    amount_quote: (u128::from(rng % 40) + 1) * Q,
                },
                1 => Command::Contribute {
                    actor: Actor::Owner(OwnerId(10 + id.0)),
                    member: id,
                    amount_quote: Q,
                },
                2 => Command::FundCell {
                    actor: Actor::Delegate(D),
                    cell: CellId(1),
                    amount_quote: Q,
                },
                3 => {
                    let mut c = open_command(&room, id);
                    if let Command::Open { intent, .. } = &mut c {
                        intent.consent.nonce = intent.consent.nonce.wrapping_add(1);
                    }
                    c
                }
                4 => {
                    let p = (1..=64).find_map(|i| room.position(PositionId(i)).cloned());
                    match p {
                        Some(p) => close_command(
                            &room,
                            &p,
                            PRICE_SCALE * (u128::from(rng % 19) + 1) / 10,
                            11,
                        ),
                        None => open_command(&room, id),
                    }
                }
                _ => open_command(&room, id),
            };
            let before = room.clone();
            if room.execute(command).is_err() {
                assert_eq!(room, before, "seed {seed}, step {step}");
            }
            room.check_invariants().unwrap();
        }
        assert_eq!(
            Room::replay(RoomId(1), OwnerId(1), room.records()).unwrap(),
            room
        );
    }
}

#[test]
fn backing_owner_opt_in_blocks_foreign_members_and_preclaimed_identities() {
    let mut room = fixture();
    // Member999's own consent cannot authorize somebody else's backing.
    room.execute(Command::AddMember {
        actor: Actor::Owner(OwnerId(999)),
        member: MemberId(999),
        delegate: Some(D),
        policy: policy(),
    })
    .unwrap();
    room.execute(Command::Contribute {
        actor: Actor::Owner(OwnerId(999)),
        member: MemberId(999),
        amount_quote: 100 * Q,
    })
    .unwrap();
    let attack = open_command(&room, MemberId(999));
    rejects(&mut room, attack, Error::UnderwritingDenied);
    // A forward admission names the expected owner/incarnation too: first-come
    // ID registration cannot turn someone else's intended admission into ours.
    let mut promised = underwriting();
    promised
        .allowed_members
        .insert((MemberId(999), OwnerId(500), 1));
    room.execute(Command::ChangeCellPolicy {
        actor: Actor::Owner(OwnerId(101)),
        cell: CellId(1),
        expected_revision: 1,
        policy: promised,
    })
    .unwrap();
    let attack = open_command(&room, MemberId(999));
    rejects(&mut room, attack, Error::UnderwritingDenied);
    let command = open_command(&room, MemberId(1));
    opened(&mut room, command);
}

#[test]
fn underwriting_terms_and_revision_bind_every_future_entry() {
    let restrictions: Vec<fn(&mut UnderwritingPolicy)> = vec![
        |p| p.allowed_members.clear(),
        |p| p.allowed_members = BTreeSet::from([(MemberId(1), OwnerId(11), 2)]),
        |p| p.allowed_market_generations = BTreeSet::from([(MarketId(1), 2)]),
        |p| p.max_position_notional_quote = 99 * Q,
        |p| p.max_profit_cap_quote = 49 * Q,
        |p| p.max_leverage_bps = 39_999,
        |p| p.max_total_reserved_quote = 74 * Q,
        |p| p.min_fee_bps = 11,
    ];
    for restrict in restrictions {
        let mut room = fixture();
        let old_intent = open_command(&room, MemberId(1));
        let mut p = underwriting();
        restrict(&mut p);
        room.execute(Command::ChangeCellPolicy {
            actor: Actor::Owner(OwnerId(101)),
            cell: CellId(1),
            expected_revision: 1,
            policy: p,
        })
        .unwrap();
        rejects(&mut room, old_intent, Error::WrongCellPolicy);
        let command = open_command(&room, MemberId(1));
        rejects(&mut room, command, Error::UnderwritingDenied);
    }
    let mut room = fixture();
    for actor in [Actor::Delegate(D), Actor::Owner(OwnerId(11))] {
        rejects(
            &mut room,
            Command::ChangeCellPolicy {
                actor,
                cell: CellId(1),
                expected_revision: 1,
                policy: underwriting(),
            },
            Error::Unauthorized,
        );
    }
    rejects(
        &mut room,
        Command::ChangeCellPolicy {
            actor: Actor::Owner(OwnerId(101)),
            cell: CellId(1),
            expected_revision: 2,
            policy: underwriting(),
        },
        Error::WrongCellPolicy,
    );
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    assert_eq!(p.cell_policy, underwriting());
    assert_eq!(p.intent.cell_policy_revision, 1);
    let mut limited = underwriting();
    limited.max_total_reserved_quote = 100 * Q;
    room.execute(Command::ChangeCellPolicy {
        actor: Actor::Owner(OwnerId(101)),
        cell: CellId(1),
        expected_revision: 1,
        policy: limited,
    })
    .unwrap();
    let command = open_command(&room, MemberId(2));
    rejects(&mut room, command, Error::UnderwritingDenied);
    assert_eq!(room.position(p.position), Some(&p));
    assert_eq!(room.cell(CellId(1)).unwrap().reserved_quote, 75 * Q);
}

#[test]
fn permissionless_maturity_uses_immutable_terminal_price_and_needs_no_trader() {
    let mut room = fixture();
    let command = open_command(&room, MemberId(1));
    let p = opened(&mut room, command);
    let command = open_command(&room, MemberId(2));
    let second = opened(&mut room, command);
    let old_close = close_command(&room, &p, 2 * PRICE_SCALE, 100);
    let nonce = room.member(MemberId(1)).unwrap().next_nonce;
    let original_cell = room.cell(CellId(1)).unwrap().clone();
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        expected_revision: 1,
        delegate: None,
        policy: policy(),
    })
    .unwrap();
    let mut paused = underwriting();
    paused.allowed_members.clear();
    paused.max_total_reserved_quote = 1; // Does not release grandfathered reserves.
    room.execute(Command::ChangeCellPolicy {
        actor: Actor::Owner(OwnerId(101)),
        cell: CellId(1),
        expected_revision: 1,
        policy: paused,
    })
    .unwrap();
    room.execute(Command::RegisterMarket {
        actor: Actor::Owner(OwnerId(1)),
        market: Market {
            generation: 2,
            min_price: 3 * PRICE_SCALE,
            ..market()
        },
    })
    .unwrap();
    assert_eq!(room.position(p.position), Some(&p));
    assert_eq!(
        room.cell(CellId(1)).unwrap().reserved_quote,
        original_cell.reserved_quote
    );
    rejects(
        &mut room,
        Command::SettleMatured {
            position: p.position,
            now_slot: 99,
        },
        Error::InvalidTime,
    );
    rejects(
        &mut room,
        Command::SettleMatured {
            position: p.position,
            now_slot: 100,
        },
        Error::MissingMaturityObservation,
    );
    let terminal = Command::RecordTerminalObservation {
        actor: Actor::Owner(OwnerId(1)),
        position: p.position,
        observation: quote(2 * PRICE_SCALE, 100),
        now_slot: 200,
    };
    let mut bad = terminal.clone();
    if let Command::RecordTerminalObservation { actor, .. } = &mut bad {
        *actor = Actor::Delegate(D);
    }
    rejects(&mut room, bad, Error::Unauthorized);
    let mut bad = terminal.clone();
    if let Command::RecordTerminalObservation { observation, .. } = &mut bad {
        observation.published_slot = 101;
    }
    rejects(&mut room, bad, Error::MissingMaturityObservation);
    let mut bad = terminal.clone();
    if let Command::RecordTerminalObservation { observation, .. } = &mut bad {
        observation.generation = 2;
    }
    rejects(&mut room, bad, Error::WrongGeneration);
    let mut bad = terminal.clone();
    if let Command::RecordTerminalObservation { now_slot, .. } = &mut bad {
        *now_slot = 99;
    }
    rejects(&mut room, bad, Error::InvalidTime);
    // The original generation's in-band price remains publishable after rotation.
    room.execute(terminal.clone()).unwrap();
    let mut replacement = terminal;
    if let Command::RecordTerminalObservation { observation, .. } = &mut replacement {
        observation.price = PRICE_SCALE;
    }
    rejects(&mut room, replacement, Error::AlreadyExists);
    // A still-authorized member cannot pick a different quote for the same maturity.
    let mut forged = close_command(&room, &second, PRICE_SCALE, 200);
    if let Command::Close { observation, .. } = &mut forged {
        observation.published_slot = 100;
    }
    rejects(&mut room, forged, Error::MissingMaturityObservation);
    let settle = Command::SettleMatured {
        position: p.position,
        now_slot: 200,
    };
    let Outcome::Closed(closed) = room.execute(settle.clone()).unwrap() else {
        panic!()
    };
    assert_eq!(closed.owner, OwnerId(11));
    assert_eq!(closed.member, MemberId(1));
    assert_eq!(closed.observation, quote(2 * PRICE_SCALE, 100));
    assert_eq!(closed.payout_quote, p.payout_at(2 * PRICE_SCALE).unwrap());
    assert_eq!(room.member(MemberId(1)).unwrap().next_nonce, nonce);
    assert_eq!(
        room.cell(CellId(1)).unwrap().reserved_quote,
        second.maximum_payout_quote
    );
    rejects(&mut room, settle, Error::UnknownPosition);
    // Old signed/typed closes are harmless, regardless of their prior validity.
    let mut old_close = old_close;
    if let Command::Close {
        actor, now_slot, ..
    } = &mut old_close
    {
        *actor = Actor::Owner(OwnerId(11));
        *now_slot = 200;
    }
    rejects(&mut room, old_close, Error::UnknownPosition);
    let Outcome::Closed(other) = room
        .execute(Command::SettleMatured {
            position: second.position,
            now_slot: 200,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(other.observation, closed.observation);
    assert_eq!(room.cell(CellId(1)).unwrap().reserved_quote, 0);
    assert_eq!(
        Room::replay(RoomId(1), OwnerId(1), room.records()).unwrap(),
        room
    );
}
