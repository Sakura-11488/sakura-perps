//! Authenticated, in-memory command gateway for the Risk Rooms paper model.
//!
//! Signatures authenticate instructions, not collateral transfers, oracle truth,
//! operator time, durable storage or consensus. No wallet or network is accessed.

#![forbid(unsafe_code)]

pub mod wire;

use std::collections::{BTreeMap, BTreeSet};

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Signature, VerifyingKey};
use sakura_perps_rooms::{Actor, Command, Observation, Outcome, PositionId, Room};
use sha2::{Digest, Sha256};
use wire::{Action, Genesis, Intent, SignedEnvelope, WireError};

pub const MAX_PRINCIPALS: usize = 128;
pub const MAX_POLICY_ENTRIES: usize = 64;
/// Applies to every signed operation, including policy and funding commands.
pub const MAX_AUTH_LIFETIME_SLOTS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Wire(WireError),
    Kernel(sakura_perps_rooms::Error),
    InvalidGenesis,
    UnknownPrincipal,
    InvalidSignature,
    WrongDomain,
    WrongSequence,
    InvalidTime,
    WrongAuthorizationRevision,
    UnexpectedContext,
    LimitExceeded,
    Arithmetic,
    TranscriptMismatch,
}

impl From<WireError> for Error {
    fn from(value: WireError) -> Self {
        Self::Wire(value)
    }
}
impl From<sakura_perps_rooms::Error> for Error {
    fn from(value: sakura_perps_rooms::Error) -> Self {
        Self::Kernel(value)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Supplied by the trusted execution adapter, never taken from the signed request.
/// A deployment must independently authenticate both its clock and price source.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ExecutionContext {
    pub now_slot: u64,
    pub observation: Option<Observation>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum AuthOutcome {
    Kernel(Outcome),
    NonceAdvanced { actor: Actor, next_sequence: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Evidence {
    Signed {
        envelope: Box<SignedEnvelope>,
        context: ExecutionContext,
    },
    /// No signer, beneficiary or price is supplied by the settlement requester.
    Matured { position: PositionId, now_slot: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct AuthenticatedRecord {
    pub index: u64,
    pub previous_hash: [u8; 32],
    pub evidence: Evidence,
    pub outcome: AuthOutcome,
    pub hash: [u8; 32],
}

/// Immutable genesis, verified command nonces and the economic state move together.
/// There is no mutable kernel accessor, key-rebinding method or unsigned owner path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedRoom {
    genesis: Genesis,
    genesis_hash: [u8; 32],
    keys: BTreeMap<Actor, VerifyingKey>,
    sequences: BTreeMap<Actor, u64>,
    room: Room,
    current_slot: u64,
    records: Vec<AuthenticatedRecord>,
    head: [u8; 32],
}

impl AuthenticatedRoom {
    pub fn new(genesis: Genesis) -> Result<Self, Error> {
        if genesis.principals.is_empty()
            || genesis.principals.len() > MAX_PRINCIPALS
            || genesis.domain.network == [0; 32]
            || genesis.domain.deployment == [0; 32]
        {
            return Err(Error::InvalidGenesis);
        }
        let mut keys = BTreeMap::new();
        let mut public_keys = BTreeSet::new();
        let mut previous = None;
        for binding in &genesis.principals {
            if previous.is_some_and(|actor| actor >= binding.actor)
                || !public_keys.insert(binding.public_key)
            {
                return Err(Error::InvalidGenesis);
            }
            let key =
                VerifyingKey::from_bytes(&binding.public_key).map_err(|_| Error::InvalidGenesis)?;
            // Restrict identities to canonical prime-order signing keys. Strict
            // verification additionally rejects weak/malleable signatures.
            if key.is_weak()
                || !key.to_edwards().is_torsion_free()
                || key.to_edwards().compress().to_bytes() != binding.public_key
            {
                return Err(Error::InvalidGenesis);
            }
            keys.insert(binding.actor, key);
            previous = Some(binding.actor);
        }
        if !keys.contains_key(&Actor::Owner(genesis.administrator)) {
            return Err(Error::InvalidGenesis);
        }
        let genesis_hash = Sha256::digest(wire::genesis_bytes(&genesis)?).into();
        let sequences = keys.keys().map(|actor| (*actor, 0)).collect();
        let room = Room::new(genesis.domain.room, genesis.administrator);
        Ok(Self {
            genesis,
            genesis_hash,
            keys,
            sequences,
            room,
            current_slot: 0,
            records: Vec::new(),
            head: genesis_hash,
        })
    }

    pub fn genesis(&self) -> &Genesis {
        &self.genesis
    }
    pub fn genesis_hash(&self) -> [u8; 32] {
        self.genesis_hash
    }
    pub fn head_hash(&self) -> [u8; 32] {
        self.head
    }
    pub fn next_sequence(&self, actor: Actor) -> Option<u64> {
        self.sequences.get(&actor).copied()
    }
    pub fn current_slot(&self) -> u64 {
        self.current_slot
    }
    pub fn room(&self) -> &Room {
        &self.room
    }
    pub fn records(&self) -> &[AuthenticatedRecord] {
        &self.records
    }

    pub fn execute(
        &mut self,
        bytes: &[u8],
        context: ExecutionContext,
    ) -> Result<AuthenticatedRecord, Error> {
        let envelope: SignedEnvelope = wire::decode(bytes)?;
        let intent = &envelope.intent;
        if intent.genesis_hash != self.genesis_hash {
            return Err(Error::WrongDomain);
        }
        let key = self
            .keys
            .get(&intent.actor)
            .ok_or(Error::UnknownPrincipal)?;
        key.verify_strict(
            &wire::signing_bytes(intent)?,
            &Signature::from_bytes(&envelope.signature),
        )
        .map_err(|_| Error::InvalidSignature)?;
        if self.next_sequence(intent.actor) != Some(intent.sequence) {
            return Err(Error::WrongSequence);
        }
        let next_sequence = intent.sequence.checked_add(1).ok_or(Error::Arithmetic)?;
        self.check_time(intent, context.now_slot)?;
        self.check_action(intent)?;
        let command = Self::command(intent, &context)?;
        let mut planned = self.clone();
        let outcome = match command {
            Some(command) => AuthOutcome::Kernel(planned.room.execute(command)?),
            None => AuthOutcome::NonceAdvanced {
                actor: intent.actor,
                next_sequence,
            },
        };
        planned.sequences.insert(intent.actor, next_sequence);
        planned.current_slot = context.now_slot;
        let record = planned.append(
            Evidence::Signed {
                envelope: Box::new(envelope),
                context,
            },
            outcome,
        )?;
        *self = planned;
        Ok(record)
    }

    /// Anyone can request maturity settlement. The adapter supplies current time;
    /// the kernel chooses the immutable stored observation and stored beneficiary.
    pub fn settle_matured(
        &mut self,
        position: PositionId,
        now_slot: u64,
    ) -> Result<AuthenticatedRecord, Error> {
        if now_slot < self.current_slot {
            return Err(Error::InvalidTime);
        }
        let mut planned = self.clone();
        let outcome = planned
            .room
            .execute(Command::SettleMatured { position, now_slot })?;
        planned.current_slot = now_slot;
        let record = planned.append(
            Evidence::Matured { position, now_slot },
            AuthOutcome::Kernel(outcome),
        )?;
        *self = planned;
        Ok(record)
    }

    /// Replay verifies signatures and transitions as well as record hashes. The
    /// expected head MUST come from an independent trusted checkpoint; a supplied
    /// history plus its own replacement head does not establish history integrity.
    pub fn replay(
        genesis: Genesis,
        records: &[AuthenticatedRecord],
        expected_head: [u8; 32],
    ) -> Result<Self, Error> {
        let mut rebuilt = Self::new(genesis)?;
        for record in records {
            let generated = match &record.evidence {
                Evidence::Signed { envelope, context } => {
                    rebuilt.execute(&wire::encode(envelope)?, context.clone())?
                }
                Evidence::Matured { position, now_slot } => {
                    rebuilt.settle_matured(*position, *now_slot)?
                }
            };
            if &generated != record {
                return Err(Error::TranscriptMismatch);
            }
        }
        if rebuilt.head != expected_head {
            return Err(Error::TranscriptMismatch);
        }
        Ok(rebuilt)
    }

    fn check_time(&self, intent: &Intent, now: u64) -> Result<(), Error> {
        if now < self.current_slot
            || intent.expires_slot < intent.not_before_slot
            || intent.expires_slot - intent.not_before_slot > MAX_AUTH_LIFETIME_SLOTS
            || now < intent.not_before_slot
            || now > intent.expires_slot
        {
            return Err(Error::InvalidTime);
        }
        Ok(())
    }

    fn check_action(&self, intent: &Intent) -> Result<(), Error> {
        let consent = match &intent.action {
            Action::Open { intent } => Some(&intent.consent),
            Action::Close { intent } => Some(&intent.consent),
            _ => None,
        };
        if let Some(consent) = consent {
            let member = self
                .room
                .member(consent.member)
                .ok_or(sakura_perps_rooms::Error::UnknownMember)?;
            if intent.authorization_revision != Some(member.policy_revision) {
                return Err(Error::WrongAuthorizationRevision);
            }
        } else if intent.authorization_revision.is_some() {
            return Err(Error::WrongAuthorizationRevision);
        }
        match &intent.action {
            Action::AddMember {
                delegate, policy, ..
            }
            | Action::ChangePolicy {
                delegate, policy, ..
            } => {
                if policy.allowed_markets.len() > MAX_POLICY_ENTRIES
                    || policy.allowed_cells.len() > MAX_POLICY_ENTRIES
                {
                    return Err(Error::LimitExceeded);
                }
                if delegate.is_some_and(|id| !self.keys.contains_key(&Actor::Delegate(id))) {
                    return Err(Error::UnknownPrincipal);
                }
            }
            Action::AddCell { policy, .. } | Action::ChangeCellPolicy { policy, .. } => {
                if policy.allowed_members.len() > MAX_POLICY_ENTRIES
                    || policy.allowed_market_generations.len() > MAX_POLICY_ENTRIES
                {
                    return Err(Error::LimitExceeded);
                }
                if policy
                    .allowed_members
                    .iter()
                    .any(|(_, owner, _)| !self.keys.contains_key(&Actor::Owner(*owner)))
                {
                    return Err(Error::UnknownPrincipal);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn command(intent: &Intent, context: &ExecutionContext) -> Result<Option<Command>, Error> {
        let actor = intent.actor;
        let now_slot = context.now_slot;
        let uses_observation = matches!(intent.action, Action::Open { .. } | Action::Close { .. });
        if uses_observation != context.observation.is_some() {
            return Err(Error::UnexpectedContext);
        }
        Ok(Some(match &intent.action {
            Action::RegisterMarket { market } => Command::RegisterMarket {
                actor,
                market: market.clone(),
            },
            Action::AddCell { cell, policy } => Command::AddCell {
                actor,
                cell: *cell,
                policy: policy.clone(),
            },
            Action::ChangeCellPolicy {
                cell,
                expected_revision,
                policy,
            } => Command::ChangeCellPolicy {
                actor,
                cell: *cell,
                expected_revision: *expected_revision,
                policy: policy.clone(),
            },
            Action::AddMember {
                member,
                delegate,
                policy,
            } => Command::AddMember {
                actor,
                member: *member,
                delegate: *delegate,
                policy: policy.clone(),
            },
            Action::ChangePolicy {
                member,
                expected_revision,
                delegate,
                policy,
            } => Command::ChangePolicy {
                actor,
                member: *member,
                expected_revision: *expected_revision,
                delegate: *delegate,
                policy: policy.clone(),
            },
            Action::Contribute {
                member,
                amount_quote,
            } => Command::Contribute {
                actor,
                member: *member,
                amount_quote: *amount_quote,
            },
            Action::FundCell { cell, amount_quote } => Command::FundCell {
                actor,
                cell: *cell,
                amount_quote: *amount_quote,
            },
            Action::Withdraw {
                member,
                amount_quote,
            } => Command::Withdraw {
                actor,
                member: *member,
                amount_quote: *amount_quote,
            },
            Action::WithdrawCell { cell, amount_quote } => Command::WithdrawCell {
                actor,
                cell: *cell,
                amount_quote: *amount_quote,
            },
            Action::Open { intent } => Command::Open {
                actor,
                intent: intent.clone(),
                observation: context
                    .observation
                    .clone()
                    .ok_or(Error::UnexpectedContext)?,
                now_slot,
            },
            Action::Close { intent } => Command::Close {
                actor,
                intent: intent.clone(),
                observation: context
                    .observation
                    .clone()
                    .ok_or(Error::UnexpectedContext)?,
                now_slot,
            },
            Action::RecordTerminalObservation {
                position,
                observation,
            } => Command::RecordTerminalObservation {
                actor,
                position: *position,
                observation: observation.clone(),
                now_slot,
            },
            Action::CancelPending => return Ok(None),
        }))
    }

    fn append(
        &mut self,
        evidence: Evidence,
        outcome: AuthOutcome,
    ) -> Result<AuthenticatedRecord, Error> {
        let index = u64::try_from(self.records.len()).map_err(|_| Error::Arithmetic)?;
        // Canonical tuple encoding is length-delimited and enum-tagged. No Debug,
        // JSON, omitted fields or concatenated variable strings are signed/hashed.
        let body = wire::encode(&(index, self.head, &evidence, &outcome))?;
        let mut digest = Sha256::new();
        digest.update(wire::RECEIPT_DOMAIN);
        digest.update(wire::WIRE_VERSION.to_le_bytes());
        digest.update(body);
        let hash = digest.finalize().into();
        let record = AuthenticatedRecord {
            index,
            previous_hash: self.head,
            evidence,
            outcome,
            hash,
        };
        // Ensure the public record format itself fits the decoder's byte bound.
        wire::encode(&record)?;
        self.records.push(record.clone());
        self.head = hash;
        Ok(record)
    }
}
