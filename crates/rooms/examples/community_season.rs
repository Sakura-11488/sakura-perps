//! Run with: cargo run -p sakura-perps-rooms --example community_season --locked
use sakura_perps_rooms::*;
use std::collections::BTreeSet;

const Q: u128 = 1_000_000;

fn policy(cell: CellId) -> Policy {
    Policy {
        contribution_limit_quote: 30 * Q,
        loss_limit_quote: 30 * Q,
        allowed_markets: BTreeSet::from([MarketId(1)]),
        allowed_cells: BTreeSet::from([cell]),
        max_leverage_bps: 20_000,
        max_position_notional_quote: 100 * Q,
        max_total_notional_quote: 100 * Q,
        max_open_positions: 2,
        max_fee_bps: 10,
        max_intent_lifetime_slots: 10,
    }
}

fn consent(room: &Room, member: MemberId, now: u64) -> Consent {
    let m = room.member(member).expect("scenario member");
    Consent {
        room: RoomId(7),
        member,
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

fn quote(price: u128, slot: u64) -> Observation {
    Observation {
        market: MarketId(1),
        generation: 1,
        price,
        published_slot: slot,
    }
}

fn open(room: &Room, member: u64, cell: u64, delegate: u64, side: Side) -> Command {
    Command::Open {
        actor: Actor::Delegate(DelegateId(delegate)),
        intent: OpenIntent {
            consent: consent(room, MemberId(member), 10),
            market: MarketId(1),
            cell: CellId(cell),
            cell_policy_revision: room.cell(CellId(cell)).unwrap().policy_revision,
            side,
            size_base: 20 * Q,
            collateral_quote: 10 * Q,
            profit_cap_quote: 20 * Q,
            max_fee_quote: Q,
            maturity_slot: 50,
        },
        observation: quote(PRICE_SCALE, 10),
        now_slot: 10,
    }
}

fn expect_rejection(room: &mut Room, label: &str, command: Command, expected: Error) {
    let before = room.clone();
    assert_eq!(room.execute(command), Err(expected.clone()), "{label}");
    assert_eq!(*room, before, "failed action changed room state");
    println!("rejected: {label}: {expected:?}; all state unchanged");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Risk Rooms: PAPER SIMULATION ONLY; asserted identities, no signatures or tokens.");
    println!("Two teams coordinate finite capped contracts; all amounts below are quote atoms.");
    let mut room = Room::new(RoomId(7), OwnerId(1));
    room.execute(Command::RegisterMarket {
        actor: Actor::Owner(OwnerId(1)),
        market: Market {
            id: MarketId(1),
            generation: 1,
            asset_decimals: 6,
            min_price: PRICE_SCALE / 10,
            max_price: 10 * PRICE_SCALE,
            max_oracle_age_slots: 2,
            max_position_lifetime_slots: 50,
            fee_bps: 10,
        },
    })?;
    for cell in 1..=2 {
        room.execute(Command::AddCell {
            actor: Actor::Owner(OwnerId(100 + cell)),
            cell: CellId(cell),
            policy: UnderwritingPolicy {
                allowed_members: if cell == 1 {
                    BTreeSet::from([(MemberId(1), OwnerId(11), 1), (MemberId(2), OwnerId(12), 1)])
                } else {
                    BTreeSet::from([(MemberId(3), OwnerId(13), 1), (MemberId(4), OwnerId(14), 1)])
                },
                allowed_market_generations: BTreeSet::from([(MarketId(1), 1)]),
                max_position_notional_quote: 100 * Q,
                max_profit_cap_quote: 20 * Q,
                max_leverage_bps: 20_000,
                max_total_reserved_quote: 100 * Q,
                min_fee_bps: 10,
            },
        })?;
        room.execute(Command::FundCell {
            actor: Actor::Owner(OwnerId(100 + cell)),
            cell: CellId(cell),
            amount_quote: 100 * Q,
        })?;
    }
    for member in 1..=4 {
        let team = if member <= 2 { 1 } else { 2 };
        room.execute(Command::AddMember {
            actor: Actor::Owner(OwnerId(10 + member)),
            member: MemberId(member),
            delegate: Some(DelegateId(90 + team)),
            policy: policy(CellId(team)),
        })?;
        room.execute(Command::Contribute {
            actor: Actor::Owner(OwnerId(10 + member)),
            member: MemberId(member),
            amount_quote: 30 * Q,
        })?;
    }
    expect_rejection(
        &mut room,
        "delegate withdrawal",
        Command::Withdraw {
            actor: Actor::Delegate(DelegateId(91)),
            member: MemberId(1),
            amount_quote: Q,
        },
        Error::Unauthorized,
    );
    expect_rejection(
        &mut room,
        "delegate external top-up",
        Command::Contribute {
            actor: Actor::Delegate(DelegateId(91)),
            member: MemberId(1),
            amount_quote: Q,
        },
        Error::Unauthorized,
    );
    expect_rejection(
        &mut room,
        "delegate policy rewrite",
        Command::ChangePolicy {
            actor: Actor::Delegate(DelegateId(91)),
            member: MemberId(1),
            expected_revision: 1,
            delegate: Some(DelegateId(91)),
            policy: policy(CellId(1)),
        },
        Error::Unauthorized,
    );
    let mut invalid = open(&room, 1, 1, 91, Side::Long);
    if let Command::Open { intent, .. } = &mut invalid {
        intent.consent.policy_revision = 2;
    }
    expect_rejection(
        &mut room,
        "forged typed consent revision (no signature verification)",
        invalid,
        Error::WrongPolicy,
    );
    let mut stale = open(&room, 1, 1, 91, Side::Long);
    if let Command::Open { observation, .. } = &mut stale {
        observation.published_slot = 1;
    }
    expect_rejection(
        &mut room,
        "oracle outage at entry",
        stale,
        Error::StaleOracle,
    );
    let unauthorized_team = open(&room, 3, 2, 91, Side::Short);
    expect_rejection(
        &mut room,
        "Dawn delegate spending Dusk member budget",
        unauthorized_team,
        Error::Unauthorized,
    );

    let dawn_command = open(&room, 1, 1, 91, Side::Long);
    let Outcome::Opened(dawn) = room.execute(dawn_command.clone())? else {
        unreachable!()
    };
    expect_rejection(
        &mut room,
        "used intent replay",
        dawn_command,
        Error::WrongNonce,
    );
    let Outcome::Opened(dawn_second) = room.execute(open(&room, 2, 1, 91, Side::Long))? else {
        unreachable!()
    };
    let Outcome::Opened(dusk) = room.execute(open(&room, 3, 2, 92, Side::Short))? else {
        unreachable!()
    };
    println!("Dawn members retain separate budgets; their delegate opened two independently owned positions.");
    println!(
        "cell 1 reserves {}; cell 2 reserves {}",
        room.cell(CellId(1)).unwrap().reserved_quote,
        room.cell(CellId(2)).unwrap().reserved_quote
    );
    let dusk_cell_before = room.cell(CellId(2)).unwrap().clone();
    let close_dawn = Command::Close {
        actor: Actor::Delegate(DelegateId(91)),
        intent: CloseIntent {
            consent: consent(&room, MemberId(1), 12),
            position: dawn.position,
        },
        observation: quote(2 * PRICE_SCALE, 12),
        now_slot: 12,
    };
    let Outcome::Closed(closed) = room.execute(close_dawn)? else {
        unreachable!()
    };
    assert_eq!(closed.payout_quote, dawn.payout_at(2 * PRICE_SCALE)?);
    assert_eq!(room.cell(CellId(2)), Some(&dusk_cell_before));
    println!(
        "Dawn capped payout {}; Dusk backing cell remained identical.",
        closed.payout_quote
    );

    // The owner revokes the delegate, then leaves. Permissionless settlement
    // still returns the exact admitted payout to that member alone.
    room.execute(Command::ChangePolicy {
        actor: Actor::Owner(OwnerId(13)),
        member: MemberId(3),
        expected_revision: 1,
        delegate: None,
        policy: policy(CellId(2)),
    })?;
    let nonce_before = room.member(MemberId(3)).unwrap().next_nonce;
    // Missing a terminal price does not authorize an invented fallback.
    let terminal = Command::SettleMatured {
        position: dusk.position,
        now_slot: 80,
    };
    expect_rejection(
        &mut room,
        "terminal oracle outage",
        terminal.clone(),
        Error::MissingMaturityObservation,
    );
    room.execute(Command::RecordTerminalObservation {
        actor: Actor::Owner(OwnerId(1)),
        position: dusk.position,
        observation: quote(2 * PRICE_SCALE, 50),
        now_slot: 80,
    })?;
    let Outcome::Closed(closed_dusk) = room.execute(terminal)? else {
        unreachable!()
    };
    assert_eq!(closed_dusk.payout_quote, 0);
    assert_eq!(closed_dusk.owner, OwnerId(13));
    assert_eq!(room.member(MemberId(3)).unwrap().next_nonce, nonce_before);
    room.execute(Command::SettleMatured {
        position: dawn_second.position,
        now_slot: 80,
    })?;
    println!("Matured positions settled without trader cooperation; trade nonce unchanged.");
    let withdraw = room.member(MemberId(1)).unwrap().free_quote;
    let output = room.execute(Command::Withdraw {
        actor: Actor::Owner(OwnerId(11)),
        member: MemberId(1),
        amount_quote: withdraw,
    })?;
    assert!(matches!(
        output,
        Outcome::Withdrawn {
            owner: OwnerId(11),
            ..
        }
    ));
    expect_rejection(
        &mut room,
        "withdrawal/profit cannot refill contribution budget",
        Command::Contribute {
            actor: Actor::Owner(OwnerId(11)),
            member: MemberId(1),
            amount_quote: 1,
        },
        Error::ContributionBudget,
    );
    room.check_invariants()?;
    assert_eq!(Room::replay(RoomId(7), OwnerId(1), room.records())?, room);
    println!(
        "replayed {} successful records exactly; atom conservation and isolated reserves hold.",
        room.records().len()
    );
    println!("No funding, continuous perp market, real-money loss guarantee, or deployment was simulated.");
    Ok(())
}
