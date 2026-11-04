use super::*;
use ed25519_dalek::{Signer, SigningKey};

// Public fixture seed, not a wallet or production credential.
fn key() -> SigningKey {
    SigningKey::from_bytes(&[37; 32])
}
fn session() -> Session {
    Session::create(
        &hex(&key().verifying_key().to_bytes()),
        &hex(&[11; 32]),
        &hex(&[22; 32]),
    )
    .unwrap()
}
fn signed(session: &mut Session, request: &str) -> Applied {
    let prepared = session.prepare(request).unwrap();
    let signature =
        key().sign(&decode_hex(&prepared.signing_bytes_hex, wire::MAX_WIRE_BYTES).unwrap());
    session
        .submit(&prepared.intent_hex, &hex(&signature.to_bytes()))
        .unwrap()
}
fn initialized() -> Session {
    let mut room = session();
    for _ in 0..4 {
        signed(&mut room, r#"{"action":"initialize"}"#);
    }
    room
}
fn allocation_request(budget: u128, leverage: u128, side: &str) -> String {
    format!(
        r#"{{"action":"allocate","budget_credits":"{budget}","leverage":"{leverage}","side":"{side}"}}"#
    )
}
fn opened(side: &str) -> Session {
    let mut room = initialized();
    let request = allocation_request(100, 3, side);
    signed(&mut room, &request);
    signed(&mut room, &request);
    room
}
fn assert_json_numbers_are_strings(value: serde_json::Value) {
    match value {
        serde_json::Value::Number(_) => {
            panic!("numeric JSON value can lose precision in JavaScript")
        }
        serde_json::Value::Array(items) => {
            items.into_iter().for_each(assert_json_numbers_are_strings)
        }
        serde_json::Value::Object(items) => items
            .into_values()
            .for_each(assert_json_numbers_are_strings),
        _ => {}
    }
}

#[test]
fn long_and_short_scenarios_replay_exactly_and_report_actual_fees_and_balances() {
    for side in ["long", "short"] {
        let mut room = opened(side);
        let preview = preview(&allocation_request(100, 3, side)).unwrap();
        assert_eq!(room.summary().unwrap().allocation.as_ref(), Some(&preview));
        signed(&mut room, r#"{"action":"run_scenario"}"#);
        let sequence = room.room.next_sequence(ACTOR);
        let result = room.settle_scenario().unwrap();
        assert_eq!(room.room.next_sequence(ACTOR), sequence);
        assert_eq!(sequence, Some(7));
        assert_eq!(result.summary.phase, "settled");
        assert_eq!(
            result.summary.balance_quote_atoms,
            preview.terminal_balance_quote_atoms
        );
        assert_eq!(result.summary.reserved_quote_atoms, "0");
        let settlement = result.summary.settlement.as_ref().unwrap();
        assert_eq!(settlement.fee_quote_atoms, preview.fee_quote_atoms);
        assert_eq!(
            settlement.net_pnl_quote_atoms,
            preview.terminal_net_pnl_quote_atoms
        );
        assert_eq!(
            settlement.payout_quote_atoms,
            preview.terminal_payout_quote_atoms
        );
        assert_eq!(
            settlement.net_pnl_quote_atoms.starts_with('-'),
            side == "short"
        );
        assert_json_numbers_are_strings(serde_json::to_value(&result).unwrap());
        room.room.room().check_invariants().unwrap();
        assert_eq!(
            Session::restore(
                &room.genesis_hex().unwrap(),
                &room.records_json().unwrap(),
                &room.checkpoint().head_hash
            )
            .unwrap(),
            room
        );
    }
}

#[test]
fn every_allowed_budget_and_leverage_includes_fees_and_preserves_exact_leverage() {
    for budget in 1..=100 {
        for leverage in 1..=3 {
            for side in ["long", "short"] {
                let quote = preview(&allocation_request(budget, leverage, side)).unwrap();
                let amount = |s: &String| s.parse::<u128>().unwrap();
                let collateral = amount(&quote.collateral_quote_atoms);
                let notional = amount(&quote.notional_quote_atoms);
                let committed = amount(&quote.committed_quote_atoms);
                assert_eq!(notional, collateral * leverage);
                assert_eq!(notional, amount(&quote.size_base_atoms) * 100);
                assert_eq!(committed, collateral + amount(&quote.fee_quote_atoms));
                assert!(committed <= budget * QUOTE_ATOMS);
                assert_eq!(
                    committed + amount(&quote.unused_quote_atoms),
                    budget * QUOTE_ATOMS
                );
                assert_eq!(amount(&quote.profit_cap_quote_atoms), collateral / 2);
                assert_eq!(
                    amount(&quote.maximum_payout_quote_atoms),
                    collateral + collateral / 2
                );
                assert!(
                    amount(&quote.terminal_payout_quote_atoms)
                        <= amount(&quote.maximum_payout_quote_atoms)
                );
            }
        }
    }
}

#[test]
fn inputs_are_bounded_typed_and_canonical() {
    for budget in [
        "0",
        "101",
        "01",
        "1.0",
        "1e0",
        "-1",
        "+1",
        " 1",
        "١",
        "",
        "999999999999999999999999",
    ] {
        assert!(preview(&format!(
            r#"{{"action":"allocate","budget_credits":"{budget}","leverage":"1","side":"long"}}"#
        ))
        .is_err());
    }
    for bad in [
        r#"{"action":"allocate","budget_credits":1,"leverage":"1","side":"long"}"#,
        r#"{"action":"allocate","budget_credits":"1","leverage":"4","side":"long"}"#,
        r#"{"action":"allocate","budget_credits":"1","leverage":"1","side":"LONG"}"#,
        r#"{"action":"initialize","ignored":true}"#,
        r#"{"action":"initialize","action":"run_scenario"}"#,
        r#"{"action":"withdraw"}"#,
    ] {
        assert!(session().prepare(bad).is_err(), "accepted {bad}");
    }
    assert_eq!(
        session().prepare(&" ".repeat(MAX_ACTION_JSON_BYTES + 1)),
        Err(Error::LimitExceeded)
    );
    assert!(Session::create(
        &hex(&key().verifying_key().to_bytes()).to_uppercase(),
        &hex(&[11; 32]),
        &hex(&[22; 32])
    )
    .is_err());
    assert!(Session::create(
        &hex(&key().verifying_key().to_bytes()),
        &hex(&[0; 32]),
        &hex(&[22; 32])
    )
    .is_err());
    assert!(Session::create(
        &hex(&key().verifying_key().to_bytes()),
        &hex(&[11; 32]),
        &hex(&[0; 32])
    )
    .is_err());
}

#[test]
fn signed_arbitrary_funding_market_terms_domains_and_time_are_rejected_atomically() {
    let room = session();
    let prepared = room.prepare(r#"{"action":"initialize"}"#).unwrap();
    let intent: Intent =
        wire::decode(&decode_hex(&prepared.intent_hex, wire::MAX_WIRE_BYTES).unwrap()).unwrap();
    for mutate in 0..6 {
        let mut changed = intent.clone();
        match mutate {
            0 => changed.sequence = 1,
            1 => changed.genesis_hash[0] ^= 1,
            2 => changed.expires_slot = 2,
            3 => changed.not_before_slot = 2,
            4 => {
                changed.action = Action::FundCell {
                    cell: CELL,
                    amount_quote: 1,
                }
            }
            5 => {
                if let Action::RegisterMarket { market } = &mut changed.action {
                    market.fee_bps = 0;
                }
            }
            _ => unreachable!(),
        }
        let signature = key().sign(&wire::signing_bytes(&changed).unwrap());
        let mut candidate = room.clone();
        assert!(candidate
            .submit(
                &hex(&wire::encode(&changed).unwrap()),
                &hex(&signature.to_bytes())
            )
            .is_err());
        assert_eq!(candidate, room);
    }
    let mut candidate = room.clone();
    assert!(candidate
        .submit(&prepared.intent_hex, &hex(&[0; 64]))
        .is_err());
    assert_eq!(candidate, room);
}

#[test]
fn allocation_cannot_be_repriced_reallocated_or_opened_twice() {
    let mut room = initialized();
    let request = allocation_request(25, 2, "long");
    signed(&mut room, &request);
    let before = room.clone();
    assert!(room.prepare(&allocation_request(26, 2, "long")).is_err());
    let prepared = room.prepare(&request).unwrap();
    let mut intent: Intent =
        wire::decode(&decode_hex(&prepared.intent_hex, wire::MAX_WIRE_BYTES).unwrap()).unwrap();
    if let Action::Open { intent } = &mut intent.action {
        intent.profit_cap_quote += 1;
    }
    let signature = key().sign(&wire::signing_bytes(&intent).unwrap());
    assert!(room
        .submit(
            &hex(&wire::encode(&intent).unwrap()),
            &hex(&signature.to_bytes())
        )
        .is_err());
    assert_eq!(room, before);
    signed(&mut room, &request);
    let opened = room.clone();
    assert!(room.prepare(&request).is_err());
    assert!(room.settle_scenario().is_err());
    assert_eq!(room, opened);
    signed(&mut room, r#"{"action":"run_scenario"}"#);
    room.settle_scenario().unwrap();
    let settled = room.clone();
    assert!(room.settle_scenario().is_err());
    assert!(room.prepare(r#"{"action":"run_scenario"}"#).is_err());
    assert_eq!(room, settled);
}

#[test]
fn restore_rejects_corruption_truncation_extra_records_and_wrong_head() {
    let mut room = opened("long");
    signed(&mut room, r#"{"action":"run_scenario"}"#);
    room.settle_scenario().unwrap();
    let genesis = room.genesis_hex().unwrap();
    let records: Vec<String> = serde_json::from_str(&room.records_json().unwrap()).unwrap();
    let head = room.checkpoint().head_hash;
    for count in 0..records.len() {
        assert!(Session::restore(&genesis, &json(&records[..count]).unwrap(), &head).is_err());
    }
    let mut corrupt = records.clone();
    let shortened = corrupt[0].len() - 2;
    corrupt[0].truncate(shortened);
    assert!(Session::restore(&genesis, &json(&corrupt).unwrap(), &head).is_err());
    let mut duplicate = records.clone();
    duplicate.push(records[0].clone());
    assert_eq!(
        Session::restore(&genesis, &json(&duplicate).unwrap(), &head),
        Err(Error::LimitExceeded)
    );
    assert!(Session::restore(&genesis, &json(&records).unwrap(), &hex(&[0; 32])).is_err());
    assert_eq!(
        Session::restore(&genesis, &" ".repeat(MAX_RECORDS_JSON_BYTES + 1), &head),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn restore_rejects_gateway_valid_transcript_outside_fixed_pilot_policy() {
    let room = session();
    let mut raw = room.room.clone();
    let mut action = setup(0).unwrap().1;
    if let Action::RegisterMarket { market } = &mut action {
        market.fee_bps = 0;
    }
    let intent = room.intent(action).unwrap();
    let envelope = SignedEnvelope {
        signature: key()
            .sign(&wire::signing_bytes(&intent).unwrap())
            .to_bytes(),
        intent,
    };
    raw.execute(
        &wire::encode(&envelope).unwrap(),
        ExecutionContext {
            now_slot: 1,
            observation: None,
        },
    )
    .unwrap();
    // Cryptographically valid kernel history is insufficient for this facade.
    assert!(
        AuthenticatedRoom::replay(raw.genesis().clone(), raw.records(), raw.head_hash()).is_ok()
    );
    let records = vec![hex(&wire::encode(&raw.records()[0]).unwrap())];
    assert!(Session::restore(
        &room.genesis_hex().unwrap(),
        &json(&records).unwrap(),
        &hex(&raw.head_hash())
    )
    .is_err());
}

#[test]
fn restore_rejects_mutated_operator_context_and_permissionless_settlement_inputs() {
    let mut room = opened("short");
    signed(&mut room, r#"{"action":"run_scenario"}"#);
    room.settle_scenario().unwrap();
    for index in [0, 5, 7] {
        let mut records = room.room.records().to_vec();
        match &mut records[index].evidence {
            Evidence::Signed { context, .. } => context.now_slot += 1,
            Evidence::Matured { now_slot, .. } => *now_slot += 1,
        }
        let encoded: Vec<String> = records
            .iter()
            .map(|r| hex(&wire::encode(r).unwrap()))
            .collect();
        assert!(Session::restore(
            &room.genesis_hex().unwrap(),
            &json(&encoded).unwrap(),
            &room.checkpoint().head_hash
        )
        .is_err());
    }
}

#[test]
fn plan_and_instance_domains_change_genesis_and_prevent_signature_reuse() {
    let original = session();
    let key_hex = hex(&key().verifying_key().to_bytes());
    let mut other_plan = Session::create(&key_hex, &hex(&[11; 32]), &hex(&[23; 32])).unwrap();
    let other_instance = Session::create(&key_hex, &hex(&[12; 32]), &hex(&[22; 32])).unwrap();
    assert_ne!(
        original.genesis_hex().unwrap(),
        other_plan.genesis_hex().unwrap()
    );
    assert_ne!(
        original.genesis_hex().unwrap(),
        other_instance.genesis_hex().unwrap()
    );
    let prepared = original.prepare(r#"{"action":"initialize"}"#).unwrap();
    let signature =
        key().sign(&decode_hex(&prepared.signing_bytes_hex, wire::MAX_WIRE_BYTES).unwrap());
    let before = other_plan.clone();
    assert!(other_plan
        .submit(&prepared.intent_hex, &hex(&signature.to_bytes()))
        .is_err());
    assert_eq!(other_plan, before);
}
