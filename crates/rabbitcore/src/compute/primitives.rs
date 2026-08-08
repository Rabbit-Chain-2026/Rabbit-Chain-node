//! Primitive identifiers for UTXO Compute v1.1.

use crate::crypto::{keccak256, Hash};
use serde::{Deserialize, Serialize};

/// Domain identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct DomainId(pub u32);

/// 游戏域（"jzz"）：RabbitChain 原生应用（山海等）专用域。
/// 游戏对象/交易在此域中与主域隔离；规则参数通过链上 Config 对象治理。
/// 与 `shanhai-onchain-mmo` 共享约定，见方案-山海原生应用.md §3.1。
pub const GAME_DOMAIN: DomainId = DomainId(0x6A7A_7A00);

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
