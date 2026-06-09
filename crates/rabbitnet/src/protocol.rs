//! Protocol message definitions.

use serde::{Deserialize, Serialize};
use rabbitcore::{account::Account, block::Block, crypto::Address, crypto::Hash};

/// Compute transaction result record synchronized across peers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SyncComputeTxRecord {
    pub tx_hash: Hash,
    pub result: serde_json::Value,
}

/// Canonical sync header payload used by header-first sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncHeader {
    pub version: u32,
    pub number: u64,
    pub hash: Hash,
    pub parent_hash: Hash,
    pub state_root: Hash,
    pub transactions_root: Hash,
    pub receipts_root: Hash,
    pub timestamp: u64,
    pub difficulty: u64,
    pub nonce: u64,
    pub coinbase: Address,
    pub mix_hash: Hash,
    pub extra_data: Vec<u8>,
}

/// Full block-body payload used by body sync.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncBlockBody {
    pub block_hash: Hash,
    pub tx_count: u32,
    pub transactions_root: Hash,
    pub receipts_root: Hash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<rabbitcore::block::BlockBody>,
}

/// State snapshot payload used by follower state/index sync.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncStateSnapshot {
    pub block_number: u64,
    pub state_root: Hash,
    pub account_count: u64,
    pub accounts: Vec<Account>,
    pub compute_txs: Vec<SyncComputeTxRecord>,
    /// Snapshot proof bytes used to bind snapshot with block hash.
    pub state_proof: Vec<u8>,
}

/// Protocol message types
#[derive(Clone, Debug)]
pub enum ProtocolMessage {
    /// Disconnect from peer
    Disconnect(String),
    /// New compute transaction announcement
    NewComputeTx(Hash),
    /// New block announcement
    NewBlock(Box<Block>),
    /// New block hash announcement
    NewBlockHash(Hash),
    /// Announce current local head height.
    AnnounceHead(u64),
    /// Request block
    GetBlock(Hash),
    /// Request headers in `[start, start + limit)`.
    SyncGetHeaders { start: u64, limit: u64 },
    /// Header response batch.
    SyncHeaders(Vec<SyncHeader>),
    /// Request a block body by hash.
    SyncGetBlockBody { block_hash: Hash },
    /// Block body response.
    SyncBlockBody(SyncBlockBody),
    /// Request snapshot summary at target block number.
    SyncGetStateSnapshot { block_number: u64 },
    /// Snapshot response.
    SyncStateSnapshot(SyncStateSnapshot),
    /// Block response
    Block(Box<Block>),
}

/// Protocol trait
pub trait Protocol: Send + Sync {
    fn handle_message(&self, message: ProtocolMessage) -> Result<(), crate::NetworkError>;
}

/// Block message
#[derive(Clone, Debug)]
pub struct BlockMessage {
    pub block: Block,
}
