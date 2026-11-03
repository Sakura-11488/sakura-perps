//! Versioned canonical bytes for the authenticated paper-room boundary.
//!
//! The codec provides bounded, exact encodings, not authorization. The wrapper
//! validates the genesis, clock, actor, sequence and signature. Changing field
//! order, enum tags or economic meaning requires a new protocol version/domain.

use std::io::{self, Write};

use borsh::{BorshDeserialize, BorshSerialize};
use sakura_perps_rooms::{
    Actor, CellId, CloseIntent, DelegateId, Market, MemberId, Observation, OpenIntent, OwnerId,
    Policy, PositionId, RoomId, UnderwritingPolicy,
};

pub const MAX_WIRE_BYTES: usize = 16 * 1024;
pub const WIRE_VERSION: u16 = 1;
pub const GENESIS_DOMAIN: &[u8] = b"SakuraRiskRooms/Genesis\0";
pub const INTENT_DOMAIN: &[u8] = b"SakuraRiskRooms/Intent\0";
pub const RECEIPT_DOMAIN: &[u8] = b"SakuraRiskRooms/Receipt\0";

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Domain {
    pub network: [u8; 32],
    pub deployment: [u8; 32],
    pub room: RoomId,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PrincipalBinding {
    pub actor: Actor,
    pub public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Genesis {
    pub domain: Domain,
    pub administrator: OwnerId,
    /// Strictly ordered by role tag and numeric ID; validated by the wrapper.
    /// Bindings are immutable for this genesis, including administrator keys.
    pub principals: Vec<PrincipalBinding>,
}

/// Stable v1 action tags are their declaration order, beginning at zero.
/// Actor and execution time come from authenticated wrapper state. Live trade
/// observations come from the host execution context, never a trader payload.
/// Permissionless maturity settlement has a separate API and no action tag.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Action {
    RegisterMarket {
        market: Market,
    },
    AddCell {
        cell: CellId,
        policy: UnderwritingPolicy,
    },
    ChangeCellPolicy {
        cell: CellId,
        expected_revision: u64,
        policy: UnderwritingPolicy,
    },
    AddMember {
        member: MemberId,
        delegate: Option<DelegateId>,
        policy: Policy,
    },
    ChangePolicy {
        member: MemberId,
        expected_revision: u64,
        delegate: Option<DelegateId>,
        policy: Policy,
    },
    Contribute {
        member: MemberId,
        amount_quote: u128,
    },
    FundCell {
        cell: CellId,
        amount_quote: u128,
    },
    Withdraw {
        member: MemberId,
        amount_quote: u128,
    },
    WithdrawCell {
        cell: CellId,
        amount_quote: u128,
    },
    Open {
        intent: OpenIntent,
    },
    Close {
        intent: CloseIntent,
    },
    RecordTerminalObservation {
        position: PositionId,
        observation: Observation,
    },
    /// Auth-only action. Advances the authenticated actor sequence without
    /// touching the economic kernel, invalidating a held request at that nonce.
    CancelPending,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Intent {
    pub genesis_hash: [u8; 32],
    pub actor: Actor,
    /// Authenticated actor sequence, distinct from the kernel's member nonce.
    pub sequence: u64,
    pub not_before_slot: u64,
    pub expires_slot: u64,
    /// Current member authorization revision for Open/Close, independent of a
    /// position's frozen economic policy revision. Other actions require None.
    pub authorization_revision: Option<u64>,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SignedEnvelope {
    pub intent: Intent,
    pub signature: [u8; 64],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    TooLarge,
    InvalidEncoding,
    NonCanonical,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "wire value exceeds the size limit",
            Self::InvalidEncoding => "invalid wire encoding",
            Self::NonCanonical => "noncanonical wire encoding",
        })
    }
}

impl std::error::Error for WireError {}

#[derive(Default)]
struct BoundedWriter {
    bytes: Vec<u8>,
    too_large: bool,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > MAX_WIRE_BYTES)
        {
            self.too_large = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "wire size limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_prefixed<T: BorshSerialize>(
    value: &T,
    domain: Option<&[u8]>,
) -> Result<Vec<u8>, WireError> {
    let mut writer = BoundedWriter::default();
    let encoded = (|| {
        if let Some(domain) = domain {
            writer.write_all(domain)?;
            WIRE_VERSION.serialize(&mut writer)?;
        }
        value.serialize(&mut writer)
    })();
    if encoded.is_err() {
        return Err(if writer.too_large {
            WireError::TooLarge
        } else {
            WireError::InvalidEncoding
        });
    }
    Ok(writer.bytes)
}

/// Encode without first allocating an unbounded serialized intermediate.
pub fn encode<T: BorshSerialize>(value: &T) -> Result<Vec<u8>, WireError> {
    encode_prefixed(value, None)
}

/// Reject oversized input before decoding, trailing bytes, and encodings that
/// change when serialized again. Strict Borsh collection ordering additionally
/// rejects duplicate and reordered map/set elements before constructing them.
pub fn decode<T: BorshDeserialize + BorshSerialize>(bytes: &[u8]) -> Result<T, WireError> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(WireError::TooLarge);
    }
    let value = T::try_from_slice(bytes).map_err(|_| WireError::InvalidEncoding)?;
    if encode(&value)? != bytes {
        return Err(WireError::NonCanonical);
    }
    Ok(value)
}

/// Ed25519 signs these complete bytes directly; this is not a custom prehash
/// signature algorithm. Signature verification belongs to the wrapper.
pub fn signing_bytes(intent: &Intent) -> Result<Vec<u8>, WireError> {
    encode_prefixed(intent, Some(INTENT_DOMAIN))
}

/// SHA-256 input for the immutable genesis commitment, including protocol domain
/// and semantic version. Registry validation is a separate wrapper operation.
pub fn genesis_bytes(genesis: &Genesis) -> Result<Vec<u8>, WireError> {
    encode_prefixed(genesis, Some(GENESIS_DOMAIN))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_perps_rooms::{Consent, Side};
    use std::collections::BTreeSet;

    fn simple_intent() -> Intent {
        Intent {
            genesis_hash: [1; 32],
            actor: Actor::Owner(OwnerId(7)),
            sequence: 9,
            not_before_slot: 11,
            expires_slot: 13,
            authorization_revision: None,
            action: Action::Withdraw {
                member: MemberId(17),
                amount_quote: 19,
            },
        }
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0);
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn v1_intent_and_signature_preimage_match_fixed_golden_bytes() {
        let expected = hex_bytes(concat!(
            "0101010101010101010101010101010101010101010101010101010101010101",
            "00", // Owner role
            "0700000000000000",
            "0900000000000000",
            "0b00000000000000",
            "0d00000000000000",
            "00", // No authorization revision for a withdrawal
            "07", // Withdraw action
            "1100000000000000",
            "13000000000000000000000000000000",
        ));
        let intent = simple_intent();
        assert_eq!(encode(&intent).unwrap(), expected);
        assert_eq!(decode::<Intent>(&expected).unwrap(), intent);
        let mut signing = b"SakuraRiskRooms/Intent\0\x01\x00".to_vec();
        signing.extend_from_slice(&expected);
        assert_eq!(signing_bytes(&intent).unwrap(), signing);
        // A signature is outside the signed Intent bytes, never recursive.
        let envelope = SignedEnvelope {
            intent,
            signature: [2; 64],
        };
        let mut expected_envelope = expected;
        expected_envelope.extend_from_slice(&[2; 64]);
        assert_eq!(encode(&envelope).unwrap(), expected_envelope);
        assert_eq!(
            decode::<SignedEnvelope>(&expected_envelope).unwrap(),
            envelope
        );
    }

    #[test]
    fn genesis_commitment_input_has_a_separate_versioned_domain() {
        let genesis = Genesis {
            domain: Domain {
                network: [0x11; 32],
                deployment: [0x22; 32],
                room: RoomId(3),
            },
            administrator: OwnerId(7),
            principals: vec![PrincipalBinding {
                actor: Actor::Owner(OwnerId(7)),
                public_key: [0x33; 32],
            }],
        };
        let mut expected = b"SakuraRiskRooms/Genesis\0\x01\x00".to_vec();
        expected.extend_from_slice(&hex_bytes(concat!(
            "1111111111111111111111111111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222222222222222222222222222",
            "0300000000000000",
            "0700000000000000",
            "01000000", // One binding
            "00",
            "0700000000000000",
            "3333333333333333333333333333333333333333333333333333333333333333",
        )));
        assert_eq!(genesis_bytes(&genesis).unwrap(), expected);
        assert_ne!(GENESIS_DOMAIN, INTENT_DOMAIN);
        assert_ne!(RECEIPT_DOMAIN, INTENT_DOMAIN);
        assert_ne!(RECEIPT_DOMAIN, GENESIS_DOMAIN);
        assert_eq!(WIRE_VERSION, 1);
    }

    #[test]
    fn cancellation_has_its_own_stable_auth_only_tag() {
        assert_eq!(encode(&Action::CancelPending).unwrap(), [12]);
        assert_eq!(decode::<Action>(&[12]).unwrap(), Action::CancelPending);
        assert!(decode::<Action>(&[13]).is_err());
    }

    #[test]
    fn truncated_trailing_and_unknown_tags_are_rejected() {
        let bytes = encode(&SignedEnvelope {
            intent: simple_intent(),
            signature: [2; 64],
        })
        .unwrap();
        for length in 0..bytes.len() {
            assert!(
                decode::<SignedEnvelope>(&bytes[..length]).is_err(),
                "prefix {length}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            decode::<SignedEnvelope>(&trailing),
            Err(WireError::InvalidEncoding)
        );
        for (offset, tag) in [(32, 2), (65, 2), (66, 255)] {
            let mut invalid = bytes.clone();
            invalid[offset] = tag;
            assert_eq!(
                decode::<SignedEnvelope>(&invalid),
                Err(WireError::InvalidEncoding)
            );
        }
    }

    #[test]
    fn sets_require_strict_order_and_no_duplicates() {
        for members in [[2u64, 1], [1, 1]] {
            let mut bytes = 2u32.to_le_bytes().to_vec();
            for member in members {
                bytes.extend_from_slice(&member.to_le_bytes());
            }
            assert_eq!(
                decode::<BTreeSet<OwnerId>>(&bytes),
                Err(WireError::InvalidEncoding)
            );
        }
        // A hostile length cannot be satisfied from a short bounded frame.
        assert!(decode::<BTreeSet<OwnerId>>(&u32::MAX.to_le_bytes()).is_err());
        let expected = BTreeSet::from([OwnerId(1), OwnerId(2)]);
        assert_eq!(
            decode::<BTreeSet<OwnerId>>(&encode(&expected).unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn both_encoding_and_decoding_enforce_the_frame_bound() {
        let exact = vec![0u8; MAX_WIRE_BYTES - 4];
        let encoded = encode(&exact).unwrap();
        assert_eq!(encoded.len(), MAX_WIRE_BYTES);
        assert_eq!(decode::<Vec<u8>>(&encoded).unwrap(), exact);
        assert_eq!(
            encode(&vec![0u8; MAX_WIRE_BYTES - 3]),
            Err(WireError::TooLarge)
        );
        assert_eq!(
            decode::<Intent>(&vec![0; MAX_WIRE_BYTES + 1]),
            Err(WireError::TooLarge)
        );
    }

    #[test]
    fn side_tags_are_stable_without_a_risk_crate_codec_dependency() {
        let mut intent = OpenIntent {
            consent: Consent {
                room: RoomId(1),
                member: MemberId(2),
                member_generation: 1,
                policy_revision: 1,
                market_generation: 1,
                nonce: 0,
                issued_slot: 10,
                expires_slot: 20,
                min_execution_price: 1,
                max_execution_price: 100,
            },
            market: sakura_perps_rooms::MarketId(3),
            cell: CellId(4),
            cell_policy_revision: 1,
            side: Side::Long,
            size_base: 5,
            collateral_quote: 6,
            profit_cap_quote: 7,
            max_fee_quote: 8,
            maturity_slot: 30,
        };
        let long = encode(&intent).unwrap();
        assert_eq!(long[120], 0);
        intent.side = Side::Short;
        let mut short = encode(&intent).unwrap();
        assert_eq!(short[120], 1);
        assert_eq!(decode::<OpenIntent>(&short).unwrap(), intent);
        short[120] = 2;
        assert_eq!(
            decode::<OpenIntent>(&short),
            Err(WireError::InvalidEncoding)
        );
    }
}
