//! Protocol message definitions.

use rabbitcore::{account::Account, block::Block, crypto::Address, crypto::Hash};
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;
    use rabbitcore::account::U256;
    use rabbitcore::{block::BlockBody, crypto::Hash};

    #[test]
    fn test_sync_header_fields() {
        let header = SyncHeader {
            version: 1,
            number: 42,
            hash: Hash::from_bytes([0x01; 32]),
            parent_hash: Hash::from_bytes([0x02; 32]),
            state_root: Hash::from_bytes([0x03; 32]),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            timestamp: 1000,
            difficulty: 1_000_000,
            nonce: 12345,
            coinbase: Address::from_bytes([0xab; 20]),
            mix_hash: Hash::from_bytes([0x04; 32]),
            extra_data: b"test-extra".to_vec(),
        };
        assert_eq!(header.number, 42);
        assert_eq!(header.difficulty, 1_000_000);
        assert_eq!(&header.extra_data, b"test-extra");
        assert!(!header.hash.is_zero());
    }

    #[test]
    fn test_sync_block_body_roundtrip() {
        let body = BlockBody::default();
        let sync_body = SyncBlockBody {
            block_hash: Hash::from_bytes([0xaa; 32]),
            tx_count: 0,
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            body: Some(body.clone()),
        };
        let json = serde_json::to_string(&sync_body).expect("serialize");
        let deserialized: SyncBlockBody = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.block_hash, sync_body.block_hash);
        assert_eq!(deserialized.tx_count, 0);
        assert!(deserialized.body.is_some());
    }

    #[test]
    fn test_sync_state_snapshot_serde() {
        let accounts = vec![Account {
            address: Address::from_bytes([0x11; 20]),
            balance: U256::from(100u64),
            nonce: 5,
            ..Account::default()
        }];
        let mut snapshot = SyncStateSnapshot {
            block_number: 10,
            state_root: Hash::from_bytes([0x20; 32]),
            account_count: 1,
            accounts: accounts.clone(),
            compute_txs: vec![],
            state_proof: vec![1, 2, 3, 4],
        };
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let deserialized: SyncStateSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.block_number, 10);
        assert_eq!(deserialized.account_count, 1);
        assert_eq!(deserialized.accounts.len(), 1);
        assert_eq!(deserialized.accounts[0].nonce, 5);
        assert_eq!(deserialized.state_proof, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_protocol_message_variants() {
        let msg = ProtocolMessage::Disconnect("bye".into());
        match msg {
            ProtocolMessage::Disconnect(reason) => assert_eq!(reason, "bye"),
            _ => panic!("wrong variant"),
        }
        let msg = ProtocolMessage::AnnounceHead(99);
        match msg {
            ProtocolMessage::AnnounceHead(h) => assert_eq!(h, 99),
            _ => panic!("wrong variant"),
        }
        let msg = ProtocolMessage::SyncGetHeaders { start: 5, limit: 10 };
        match msg {
            ProtocolMessage::SyncGetHeaders { start, limit } => {
                assert_eq!(start, 5);
                assert_eq!(limit, 10);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_sync_compute_tx_record_serde() {
        let record = SyncComputeTxRecord {
            tx_hash: Hash::from_bytes([0xdd; 32]),
            result: serde_json::json!({"status": "ok", "gas_used": 21000}),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let deserialized: SyncComputeTxRecord =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.tx_hash, record.tx_hash);
        assert_eq!(deserialized.result["status"], "ok");
        assert_eq!(deserialized.result["gas_used"], 21000);
    }

    #[test]
    fn test_protocol_message_clone() {
        let msg = ProtocolMessage::NewBlockHash(Hash::from_bytes([0xbb; 32]));
        let cloned = msg.clone();
        match cloned {
            ProtocolMessage::NewBlockHash(h) => assert_eq!(h, Hash::from_bytes([0xbb; 32])),
            _ => panic!("clone changed variant"),
        }
    }
}
