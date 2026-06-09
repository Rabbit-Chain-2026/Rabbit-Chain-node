//! Primitive identifiers for UTXO Compute v1.1.

use crate::crypto::{keccak256, Hash};
use serde::{Deserialize, Serialize};

/// Domain identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct DomainId(pub u32);

/// Logical object identifier (stable across versions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct ObjectId(pub Hash);

/// Physical immutable output identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct OutputId(pub Hash);

/// Operation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct TxId(pub Hash);

/// Monotonic object version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Version(pub u64);

/// Resource key used by resource policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct ResourceId(pub Hash);

/// Pointer to a concrete object output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectPointer {
    /// Referenced output id.
    pub output_id: OutputId,
    /// Referenced domain.
    pub domain_id: DomainId,
}

impl ObjectId {
    /// Creates an object id from arbitrary seed bytes.
    pub fn from_seed(seed: &[u8]) -> Self {
        Self(Hash::from_bytes(keccak256(seed)))
    }
}

impl TxId {
    /// Creates tx id from serialized operation bytes.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(Hash::from_bytes(keccak256(bytes)))
    }
}
