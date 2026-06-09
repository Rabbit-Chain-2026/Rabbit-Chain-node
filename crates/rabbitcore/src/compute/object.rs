//! Object and output definitions for UTXO Compute v1.1.

use serde::{Deserialize, Serialize};

use crate::crypto::{Address, Hash};

use super::primitives::{DomainId, ObjectId, OutputId, Version};

/// Asset/resource identifier.
pub type AssetId = Hash;

/// Heterogeneous resource value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceValue {
    /// Fungible amount.
    Amount(u128),
    /// Arbitrary data payload.
    Data(Vec<u8>),
    /// Reference to another logical object.
    Ref(ObjectId),
    /// Batch references to logical objects.
    RefBatch(Vec<ObjectId>),
}

/// Deterministic resource map sorted by `AssetId`.
pub type ResourceMap = Vec<(AssetId, ResourceValue)>;

/// VM-typed script payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    /// VM type: 0=BitcoinScript, 1=WASM, etc.
    pub vm: u8,
    /// Script bytecode.
    pub code: Vec<u8>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            vm: 1,
            code: Vec::new(),
        }
    }
}

/// Object ownership model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ownership {
    /// Single externally controlled owner.
    Address(Address),
    /// Program/contract-controlled object.
    Program(Address),
    /// Shared object with no single owner authority.
    Shared,
    /// Owner controlled directly by an ed25519 public key.
    Ed25519([u8; 32]),
}

/// Supported object categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    /// Fungible value-bearing object.
    Asset,
    /// Contract/program code object.
    Code,
    /// Pure state/config object.
    State,
    /// Capability/tokenized permission object.
    Capability,
    /// Agent spec/state object.
    Agent,
    /// Cross-domain bridge object.
    Anchor,
    /// Commit-reveal proof object.
    Ticket,
}

/// Immutable versioned object output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectOutput {
    /// Physical immutable output identity.
    pub output_id: OutputId,
    /// Stable logical identity for all versions.
    pub object_id: ObjectId,
    /// Object version.
    pub version: Version,
    /// Owning domain.
    pub domain_id: DomainId,
    /// Category.
    pub kind: ObjectKind,
    /// Owner semantics.
    pub owner: Ownership,
    /// Optional predecessor output for lineage.
    pub predecessor: Option<OutputId>,
    /// Deterministic state payload.
    pub state: Vec<u8>,
    /// Optional state commitment root for large off-chain state.
    pub state_root: Option<Hash>,
    /// Resource accounting tags/values.
    pub resources: ResourceMap,
    /// Ownership/locking script.
    pub lock: Script,
    /// Optional executable logic script.
    pub logic: Option<Script>,
    /// Created-at height/timestamp.
    pub created_at: u64,
    /// Optional output TTL.
    pub ttl: Option<u64>,
    /// Optional rent reserve for lifecycle economics.
    pub rent_reserve: Option<u128>,
    /// Feature flags bitmap.
    pub flags: u32,
    /// Forward-compatible extension tuples.
    pub extensions: Vec<(String, Vec<u8>)>,
    /// Whether output has been consumed.
    pub spent: bool,
}

impl ObjectOutput {
    /// Returns true if output is still spendable.
    pub fn is_live(&self) -> bool {
        !self.spent
    }
}
