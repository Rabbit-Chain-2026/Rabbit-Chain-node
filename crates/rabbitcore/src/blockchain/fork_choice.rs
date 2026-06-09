//! Fork choice rule implementation

use super::{Blockchain, Result};
use crate::account::U256;
use crate::block::{Block, BlockHeader};
use crate::crypto::Hash;
use std::sync::Arc;

/// Fork choice strategy
pub trait ForkChoice: Send + Sync {
    /// Select best block from candidates
    fn select_best(&self, candidates: &[&BlockHeader]) -> Option<&BlockHeader>;

    /// Check if block is on canonical chain
    fn is_canonical(&self, hash: &Hash) -> bool;
}

/// GHOST (Greedy Heaviest Observed Subtree) implementation
pub struct GhostForkChoice {
    blockchain: Arc<Blockchain>,
}

impl GhostForkChoice {
    pub fn new(blockchain: Arc<Blockchain>) -> Self {
        Self { blockchain }
    }

    /// Calculate block weight
    fn calculate_weight(&self, block: &BlockHeader) -> U256 {
        // Weight = total difficulty + uncle rewards
        block.difficulty
    }
}

impl ForkChoice for GhostForkChoice {
    fn select_best(&self, candidates: &[&BlockHeader]) -> Option<&BlockHeader> {
        if candidates.is_empty() {
            return None;
        }

        // Select block with highest weight
        candidates
            .iter()
            .max_by(|a, b| {
                let weight_a = self.calculate_weight(a);
                let weight_b = self.calculate_weight(b);
                weight_a.cmp(&weight_b)
            })
            .copied()
    }

    fn is_canonical(&self, hash: &Hash) -> bool {
        // Would check if block is on canonical chain
        true
    }
}

/// Longest chain rule (simplified)
pub struct LongestChainRule {
    blockchain: Arc<Blockchain>,
}

impl LongestChainRule {
    pub fn new(blockchain: Arc<Blockchain>) -> Self {
        Self { blockchain }
    }
}

impl ForkChoice for LongestChainRule {
    fn select_best(&self, candidates: &[&BlockHeader]) -> Option<&BlockHeader> {
        // Select block with highest number
        candidates
            .iter()
            .max_by(|a, b| a.number.cmp(&b.number))
            .copied()
    }

    fn is_canonical(&self, hash: &Hash) -> bool {
        // Would check canonical chain
        true
    }
}

/// Reorg manager
pub struct ReorgManager {
    blockchain: Arc<Blockchain>,
    fork_choice: Box<dyn ForkChoice>,
}

impl ReorgManager {
    pub fn new(blockchain: Arc<Blockchain>, fork_choice: Box<dyn ForkChoice>) -> Self {
        Self {
            blockchain,
            fork_choice,
        }
    }

    /// Check if reorg needed
    pub fn needs_reorg(&self, new_block: &Block) -> Result<bool> {
        let current_best = self.blockchain.best_block();
        let new_weight = new_block.header.difficulty;
        let current_weight = current_best.header.difficulty;

        Ok(new_weight > current_weight)
    }

    /// Execute reorg
    pub fn execute_reorg(&self, new_block: &Block) -> Result<()> {
        // Would reorganize chain
        // Simplified

        Ok(())
    }

    /// Get common ancestor
    pub fn get_common_ancestor(&self, hash1: &Hash, hash2: &Hash) -> Option<Hash> {
        // Would find common ancestor
        Some(*hash1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{PowAlgorithm, PowConsensus};
    use crate::state::StateDb;

    #[test]
    fn test_ghost_fork_choice() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));

        let ghost = GhostForkChoice::new(blockchain);

        // Would test with actual blocks
    }
}
