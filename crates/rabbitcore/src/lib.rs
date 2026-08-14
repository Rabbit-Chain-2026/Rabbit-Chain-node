//! RabbitChain Core Protocol Implementation
//!
//! This crate provides the core blockchain protocol components including:
//! - Account management (hybrid balance + UTXO model)
//! - Native execution engine
//! - PoW consensus mechanism
//! - Transaction processing
//! - Block management
//! - State machine

#![allow(missing_docs)]
#![allow(rustdoc::missing_crate_level_docs)]
#![allow(unused)]
#![allow(unconditional_recursion)]
#![allow(ambiguous_glob_reexports)]
#![deny(unused_must_use)]
#![deny(rust_2018_idioms)]

pub mod account;
pub mod assets;
pub mod block;
pub mod blockchain;
pub mod compute;
pub mod consensus;
pub mod crypto;
pub mod game;
pub mod governance;
pub mod state;
pub mod trie;

// Re-export commonly used types
pub use account::{Account, AccountManager, AccountType};
pub use block::{
    Block, BlockBody, BlockBodyRecord, BlockHeader, Receipt, ReceiptLog, ReceiptStatus, TxEnvelope,
};
pub use compute::{ComputeTx, ObjectId, ObjectOutput, OutputId, TxId};
pub use consensus::{Consensus, PowAlgorithm};
pub use crypto::{Address, Ed25519PrivateKey, Ed25519PublicKey, Ed25519Signature, Hash};
pub use state::StateDb;

/// RabbitChain protocol version
pub const PROTOCOL_VERSION: u32 = 1;

/// RabbitChain chain ID (for replay protection)
pub const CHAIN_ID: u64 = 10086;

/// Maximum block gas limit
pub const MAX_BLOCK_GAS_LIMIT: u64 = 30_000_000;

/// Target block time in seconds
pub const TARGET_BLOCK_TIME: u64 = 10;

/// Maximum extra data size in bytes
pub const MAX_EXTRA_DATA_SIZE: usize = 32;

/// Genesis block number
pub const GENESIS_BLOCK_NUMBER: u64 = 0;

/// Initial block reward (in wei, 5 Rbit)
pub const INITIAL_BLOCK_REWARD: u128 = 5_000_000_000_000_000_000;

/// Halving period in blocks (approximately 4 years with 10s blocks)
pub const HALVING_PERIOD: u64 = 2_100_000;
