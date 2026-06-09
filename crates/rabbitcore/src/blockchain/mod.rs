//! Blockchain Core - Chain Management and Sync
//! 
//! Provides:
//! - Chain management
//! - Block synchronization
//! - Fork choice rule
//! - State transitions

mod chain;
mod sync;
mod fork_choice;

pub use chain::*;
pub use sync::*;
pub use fork_choice::*;

use crate::crypto::Hash;
use crate::account::U256;
use thiserror::Error;

/// Blockchain errors
#[derive(Error, Debug, Clone)]
pub enum BlockchainError {
    #[error("Block not found: {0}")]
    BlockNotFound(Hash),
    #[error("Orphan block")]
    OrphanBlock,
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Invalid state root")]
    InvalidStateRoot,
    #[error("Database error: {0}")]
    Database(String),
    #[error("Consensus error: {0}")]
    Consensus(String),
}

pub type Result<T> = std::result::Result<T, BlockchainError>;
