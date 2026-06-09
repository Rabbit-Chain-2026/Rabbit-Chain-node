//! RabbitChain Storage Layer
//!
//! Provides:
//! - Merkle Patricia Trie (MPT) for state storage
//! - Database abstraction (RocksDB/Redb)
//! - Index services for fast lookups

#![allow(missing_docs)]
#![allow(rustdoc::missing_crate_level_docs)]
#![allow(unused)]

pub mod archive;
pub mod compute;
pub mod db;
pub mod index;
pub mod trie;

pub use archive::{
    read_archive_segment, ArchiveSegment, ArchiveSegmentWriter, ArchivedBlock,
    ArchivedComputeTxResult,
};
pub use compute::ComputeStore;
pub use db::{KeyValueDB, RedbDatabase, RocksDb, RocksDbCompression};
pub use index::{BlockIndex, IndexDB, TxIndex};
pub use trie::{MerklePatriciaTrie, TrieDB, TrieNode, TrieProof};

/// Storage error types
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(String),
    #[error("Trie error: {0}")]
    Trie(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Key not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Crypto error: {0}")]
    Crypto(String),
}

impl From<rabbitcore::crypto::CryptoError> for StorageError {
    fn from(e: rabbitcore::crypto::CryptoError) -> Self {
        StorageError::Crypto(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;
