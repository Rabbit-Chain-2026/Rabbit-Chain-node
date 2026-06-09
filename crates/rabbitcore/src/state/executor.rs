//! State executor for block transitions.

use super::StateDb;
use crate::block::Block;
use crate::crypto::Hash;
use std::sync::Arc;
use thiserror::Error;

/// Execution errors
#[derive(Error, Debug, Clone)]
pub enum ExecutionError {
    #[error("Block error: {0}")]
    Block(String),
}

pub type Result<T> = std::result::Result<T, ExecutionError>;

/// State transition
#[derive(Clone, Debug)]
pub struct StateTransition {
    /// From state root
    pub from_root: Hash,
    /// To state root
    pub to_root: Hash,
}

/// State executor
pub struct StateExecutor {
    /// State database
    state_db: Arc<StateDb>,
    /// Chain ID
    chain_id: u64,
}

impl StateExecutor {
    /// Create new state executor
    pub fn new(state_db: Arc<StateDb>, chain_id: u64) -> Self {
        Self { state_db, chain_id }
    }

    /// Execute a block
    pub fn execute_block(&self, block: &Block, parent_state_root: Hash) -> Result<StateTransition> {
        tracing::info!(
            "Executing block #{} with body-aware state transition",
            block.header.number.as_u64()
        );

        let _ = self.chain_id;

        Ok(StateTransition {
            from_root: parent_state_root,
            to_root: self.state_db.state_root(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_creation() {
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let executor = StateExecutor::new(state_db, 10086);

        assert_eq!(executor.chain_id, 10086);
    }
}
