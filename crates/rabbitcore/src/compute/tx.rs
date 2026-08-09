//! Compute operation model for UTXO Compute v1.1.

use serde::{Deserialize, Serialize};

use crate::crypto::{keccak256, Hash};

use super::{
    object::{ObjectKind, Ownership, ResourceMap, ResourceValue, Script},
    primitives::{DomainId, ObjectId, OutputId, TxId, Version},
};

/// Canonical extensible metadata tuple list.
pub type Metadata = Vec<(String, Vec<u8>)>;

/// Primary compute signing domain separator for RabbitChain.
pub const RABBITCHAIN_COMPUTE_SIGNING_DOMAIN_V1: &[u8] =
    b"RABBITCHAIN-COMPUTE-SIGNING-V1";

/// Read-set reference with expected version hash binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectReadRef {
    /// Referenced output id.
    pub output_id: OutputId,
    /// Domain where referenced output resides.
    pub domain_id: DomainId,
    /// Expected object version for optimistic read validation.
    pub expected_version: Version,
}

/// Proposed output to be materialized on successful execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputProposal {
    /// Physical output id to create.
    pub output_id: OutputId,
    /// Logical object id.
    pub object_id: ObjectId,
    /// Domain of the new output.
    pub domain_id: DomainId,
    /// Kind for the new output.
    pub kind: ObjectKind,
    /// Ownership model.
    pub owner: Ownership,
    /// Optional predecessor output id (required for update semantics).
    pub predecessor: Option<OutputId>,
    /// Target version.
    pub version: Version,
    /// Deterministic state blob.
    pub state: Vec<u8>,
    /// Optional state commitment root.
    pub state_root: Option<Hash>,
    /// Resource accounting tags/values.
    pub resources: ResourceMap,
    /// Ownership/locking script.
    pub lock: Script,
    /// Optional executable logic payload.
    pub logic: Option<Script>,
    /// Created-at height/timestamp.
    pub created_at: u64,
    /// Optional output TTL.
    pub ttl: Option<u64>,
    /// Optional rent reserve.
    pub rent_reserve: Option<u128>,
    /// Feature flags bitmap.
    pub flags: u32,
    /// Forward-compatible extension tuples.
    pub extensions: Metadata,
}

/// Compute command type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Transfer ownership/value among object outputs.
    Transfer,
    /// Invoke program/logic against object state.
    Invoke,
    /// Mint new logical object and initial version.
    Mint,
    /// Burn existing object output.
    Burn,
    /// Create cross-domain anchor commitment.
    Anchor,
    /// Reveal ticket data for anchor finalization.
    Reveal,
    /// Execute scheduled agent step.
    AgentTick,
}

/// Witness/signature envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxWitness {
    /// Signatures proving authorization.
    pub signatures: Vec<TxSignature>,
    /// Optional minimal signatures required for authorization.
    pub threshold: Option<u16>,
}

/// Signature scheme used by compute witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureScheme {
    /// ed25519 signature.
    Ed25519,
}

/// One signature entry in witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxSignature {
    /// Signature scheme.
    pub scheme: SignatureScheme,
    /// Raw signature bytes (64 for ed25519).
    pub bytes: Vec<u8>,
    /// Signer public key bytes (required, 32 bytes).
    pub public_key: Option<Vec<u8>>,
}

impl TxSignature {
    /// Builds an ed25519 witness signature entry.
    pub fn ed25519(signature: [u8; 64], public_key: [u8; 32]) -> Self {
        Self {
            scheme: SignatureScheme::Ed25519,
            bytes: signature.to_vec(),
            public_key: Some(public_key.to_vec()),
        }
    }
}

/// UTXO Compute operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeTx {
    /// Deterministic tx id.
    pub tx_id: TxId,
    /// Target domain for execution.
    pub domain_id: DomainId,
    /// Command to execute.
    pub command: Command,
    /// Inputs consumed by this operation.
    pub input_set: Vec<OutputId>,
    /// Explicit read set.
    pub read_set: Vec<ObjectReadRef>,
    /// Output proposals to be committed atomically.
    pub output_proposals: Vec<OutputProposal>,
    /// Fee paid to miners in native token (legacy).
    pub fee: u64,
    /// Optional anti-replay nonce.
    pub nonce: Option<u64>,
    /// Metadata (cross-domain proofs, hints, etc.).
    pub metadata: Metadata,
    /// User payload / ABI-encoded command args.
    pub payload: Vec<u8>,
    /// Optional absolute expiration timestamp.
    pub deadline_unix_secs: Option<u64>,
    /// Optional chain identifier used for anti-replay across chains.
    pub chain_id: Option<u64>,
    /// Optional network identifier used for anti-replay across environments.
    pub network_id: Option<u32>,
    /// Authorization witness.
    pub witness: TxWitness,
    // ═══════════════════════════════════════════════════════════
    // EIP-1559 fee market fields (v1.2)
    // All amounts are denominated in **hopps** (chain base unit,
    // 1 Rbit = 10¹⁸ hopps). Carrots (10⁹ hopps) are a display unit
    // used by the RPC layer only.
    // ═══════════════════════════════════════════════════════════
    /// Maximum total fee the sender is willing to pay (hopps).
    pub max_fee: u64,
    /// Priority fee / tip for the miner, on top of the base fee (hopps).
    pub priority_fee: u64,
    /// Gas limit — maximum gas the sender will consume.
    pub gas_limit: u64,
}

impl ComputeTx {
    /// Checks minimal structural validity.
    pub fn basic_sanity_check(&self) -> bool {
        let needs_inputs = matches!(
            self.command,
            Command::Transfer | Command::Invoke | Command::Burn
        );
        let needs_outputs = !matches!(self.command, Command::Burn);

        // 纯载荷 Invoke（如山海币 MintCoin）：无输入无输出的 payload-only 交易合法，
        // 语义由域门控/执行器（gate_game_tx / economy effects）决定。
        let payload_only_invoke = matches!(self.command, Command::Invoke)
            && self.input_set.is_empty()
            && !self.payload.is_empty();

        if needs_inputs && self.input_set.is_empty() && !payload_only_invoke {
            return false;
        }

        if needs_outputs && self.output_proposals.is_empty() && !payload_only_invoke {
            return false;
        }

        for proposal in &self.output_proposals {
            if !resource_map_is_canonical(&proposal.resources) {
                return false;
            }
        }

        // Fee field sanity: a fee-paying tx must not declare a zero gas limit,
        // and a zero gas limit with a non-zero fee is contradictory.
        let fee_mode = self.max_fee > 0 || self.priority_fee > 0;
        if fee_mode && self.gas_limit == 0 {
            return false;
        }
        if !fee_mode && self.gas_limit > 0 {
            // Declaring a gas limit without any fee is allowed (legacy), but
            // must not be absurdly large.
            if self.gas_limit > crate::compute::MAX_GAS_LIMIT_PER_TX {
                return false;
            }
        }

        true
    }

    /// Deterministic signing preimage for authorization witnesses.
    ///
    /// This payload intentionally excludes witness signatures to avoid circular
    /// dependency and includes all semantically relevant fields to prevent
    /// cross-context replay and partial-tx substitution.
    pub fn signing_preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(RABBITCHAIN_COMPUTE_SIGNING_DOMAIN_V1);

        out.extend_from_slice(&self.domain_id.0.to_be_bytes());
        out.push(command_tag(self.command));

        encode_output_ids(&mut out, &self.input_set);
        encode_read_set(&mut out, &self.read_set);
        encode_output_proposals(&mut out, &self.output_proposals);
        out.extend_from_slice(&self.fee.to_be_bytes());

        match self.nonce {
            Some(nonce) => {
                out.push(1);
                out.extend_from_slice(&nonce.to_be_bytes());
            }
            None => out.push(0),
        }

        encode_metadata(&mut out, &self.metadata);

        encode_bytes(&mut out, &self.payload);

        match self.deadline_unix_secs {
            Some(deadline) => {
                out.push(1);
                out.extend_from_slice(&deadline.to_be_bytes());
            }
            None => out.push(0),
        }

        match self.chain_id {
            Some(chain_id) => {
                out.push(1);
                out.extend_from_slice(&chain_id.to_be_bytes());
            }
            None => out.push(0),
        }

        match self.network_id {
            Some(network_id) => {
                out.push(1);
                out.extend_from_slice(&network_id.to_be_bytes());
            }
            None => out.push(0),
        }

        out.extend_from_slice(&(self.witness.threshold.unwrap_or(1)).to_be_bytes());

        // EIP-1559 fee fields (v1.2+). Version marker allows backward compat.
        out.push(1); // 0=legacy, 1=v1.2 with EIP-1559 fields
        out.extend_from_slice(&self.max_fee.to_be_bytes());
        out.extend_from_slice(&self.priority_fee.to_be_bytes());
        out.extend_from_slice(&self.gas_limit.to_be_bytes());
        out
    }

    /// Keccak256 digest used for signature recovery/verification.
    pub fn signing_digest(&self) -> [u8; 32] {
        keccak256(&self.signing_preimage())
    }

    /// Canonical operation id expected from the current operation body.
    pub fn expected_tx_id(&self) -> TxId {
        TxId(Hash::from_bytes(self.signing_digest()))
    }

    /// Returns true when `tx_id` matches canonical hash of signed body.
    pub fn has_consistent_tx_id(&self) -> bool {
        self.tx_id == self.expected_tx_id()
    }

    /// Mutates operation id to the canonical expected value.
    pub fn assign_expected_tx_id(&mut self) {
        self.tx_id = self.expected_tx_id();
    }

    /// Returns a copy with canonical operation id assigned.
    pub fn with_expected_tx_id(mut self) -> Self {
        self.assign_expected_tx_id();
        self
    }
}

fn command_tag(command: Command) -> u8 {
    match command {
        Command::Transfer => 1,
        Command::Invoke => 2,
        Command::Mint => 3,
        Command::Burn => 4,
        Command::Anchor => 5,
        Command::Reveal => 6,
        Command::AgentTick => 7,
    }
}

fn object_kind_tag(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Asset => 1,
        ObjectKind::Code => 2,
        ObjectKind::State => 3,
        ObjectKind::Capability => 4,
        ObjectKind::Agent => 5,
        ObjectKind::Anchor => 6,
        ObjectKind::Ticket => 7,
    }
}

fn encode_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&(len as u32).to_be_bytes());
}

fn encode_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    encode_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn encode_output_ids(out: &mut Vec<u8>, ids: &[OutputId]) {
    encode_len(out, ids.len());
    for id in ids {
        out.extend_from_slice(id.0.as_bytes());
    }
}

fn encode_read_set(out: &mut Vec<u8>, reads: &[ObjectReadRef]) {
    encode_len(out, reads.len());
    for rr in reads {
        out.extend_from_slice(rr.output_id.0.as_bytes());
        out.extend_from_slice(&rr.domain_id.0.to_be_bytes());
        out.extend_from_slice(&rr.expected_version.0.to_be_bytes());
    }
}

fn encode_output_proposals(out: &mut Vec<u8>, proposals: &[OutputProposal]) {
    encode_len(out, proposals.len());
    for p in proposals {
        out.extend_from_slice(p.output_id.0.as_bytes());
        out.extend_from_slice(p.object_id.0.as_bytes());
        out.extend_from_slice(&p.domain_id.0.to_be_bytes());
        out.push(object_kind_tag(p.kind));
        encode_ownership(out, &p.owner);

        match p.predecessor {
            Some(pred) => {
                out.push(1);
                out.extend_from_slice(pred.0.as_bytes());
            }
            None => out.push(0),
        }

        out.extend_from_slice(&p.version.0.to_be_bytes());
        encode_bytes(out, &p.state);
        match p.state_root {
            Some(root) => {
                out.push(1);
                out.extend_from_slice(root.as_bytes());
            }
            None => out.push(0),
        }
        encode_resource_map(out, &p.resources);
        encode_script(out, &p.lock);

        match &p.logic {
            Some(logic) => {
                out.push(1);
                encode_script(out, logic);
            }
            None => out.push(0),
        }

        out.extend_from_slice(&p.created_at.to_be_bytes());
        match p.ttl {
            Some(ttl) => {
                out.push(1);
                out.extend_from_slice(&ttl.to_be_bytes());
            }
            None => out.push(0),
        }
        match p.rent_reserve {
            Some(rent) => {
                out.push(1);
                out.extend_from_slice(&rent.to_be_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&p.flags.to_be_bytes());
        encode_metadata(out, &p.extensions);
    }
}

fn encode_script(out: &mut Vec<u8>, script: &Script) {
    out.push(script.vm);
    encode_bytes(out, &script.code);
}

fn encode_resource_map(out: &mut Vec<u8>, resources: &ResourceMap) {
    encode_len(out, resources.len());
    for (asset_id, value) in resources {
        out.extend_from_slice(asset_id.as_bytes());
        match value {
            ResourceValue::Amount(v) => {
                out.push(1);
                out.extend_from_slice(&v.to_be_bytes());
            }
            ResourceValue::Data(bytes) => {
                out.push(2);
                encode_bytes(out, bytes);
            }
            ResourceValue::Ref(object_id) => {
                out.push(3);
                out.extend_from_slice(object_id.0.as_bytes());
            }
            ResourceValue::RefBatch(object_ids) => {
                out.push(4);
                encode_len(out, object_ids.len());
                for object_id in object_ids {
                    out.extend_from_slice(object_id.0.as_bytes());
                }
            }
        }
    }
}

fn encode_metadata(out: &mut Vec<u8>, metadata: &Metadata) {
    encode_len(out, metadata.len());
    for (key, value) in metadata {
        encode_bytes(out, key.as_bytes());
        encode_bytes(out, value);
    }
}

fn encode_ownership(out: &mut Vec<u8>, owner: &Ownership) {
    match owner {
        Ownership::Address(addr) => {
            out.push(1);
            out.extend_from_slice(addr.as_bytes());
        }
        Ownership::Program(addr) => {
            out.push(2);
            out.extend_from_slice(addr.as_bytes());
        }
        Ownership::Shared => {
            out.push(3);
        }
        Ownership::Ed25519(pubkey) => {
            out.push(4);
            out.extend_from_slice(pubkey);
        }
    }
}

fn resource_map_is_canonical(resources: &ResourceMap) -> bool {
    resources.windows(2).all(|pair| pair[0].0 < pair[1].0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Hash;

    fn sample_tx() -> ComputeTx {
        ComputeTx {
            tx_id: TxId(Hash::zero()),
            domain_id: DomainId(0),
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![OutputProposal {
                output_id: OutputId(Hash::from_bytes([0x11; 32])),
                object_id: ObjectId(Hash::from_bytes([0x22; 32])),
                domain_id: DomainId(0),
                kind: ObjectKind::State,
                owner: Ownership::Shared,
                predecessor: None,
                version: Version(1),
                state: vec![0x01],
                state_root: None,
                resources: vec![],
                lock: Script::default(),
                logic: None,
                created_at: 0,
                ttl: None,
                rent_reserve: None,
                flags: 0,
                extensions: vec![],
            }],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10086),
            network_id: Some(10086),
            witness: TxWitness {
                signatures: vec![],
                threshold: Some(1),
            },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        }
    }

    #[test]
    fn signing_preimage_uses_rabbitchain_prefix_and_keeps_legacy_compatibility() {
        let mut tx = sample_tx();
        tx.assign_expected_tx_id();

        let primary_preimage = tx.signing_preimage();

        assert!(primary_preimage.starts_with(RABBITCHAIN_COMPUTE_SIGNING_DOMAIN_V1));
        assert_eq!(tx.has_consistent_tx_id(), true);
        assert_eq!(tx.expected_tx_id(), tx.tx_id);
    }
}
