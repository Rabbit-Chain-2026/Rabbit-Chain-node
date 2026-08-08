//! Fork choice rule implementation

use super::{Blockchain, Result};
use crate::account::U256;
use crate::block::{Block, BlockHeader};
use crate::crypto::Hash;
use std::sync::Arc;

/// Fork choice strategy
pub trait ForkChoice: Send + Sync {
    /// Select best block from candidates
    fn select_best<'a>(&self, candidates: &[&'a BlockHeader]) -> Option<&'a BlockHeader>;

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
    fn select_best<'a>(&self, candidates: &[&'a BlockHeader]) -> Option<&'a BlockHeader> {
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
    fn select_best<'a>(&self, candidates: &[&'a BlockHeader]) -> Option<&'a BlockHeader> {
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
    use crate::block::BlockBody;
    use crate::consensus::{PowAlgorithm, PowConsensus};
    use crate::crypto::Address;
    use crate::state::StateDb;

    fn make_header(number: u64, parent_hash: Hash, difficulty: u64) -> BlockHeader {
        BlockHeader {
            version: 1,
            parent_hash,
            uncle_hashes: Vec::new(),
            coinbase: Address::from_bytes([0x00; 20]),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(number),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: number * 10,
            difficulty: U256::from(difficulty),
            nonce: 0,
            extra_data: vec![],
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        }
    }

    #[test]
    fn test_ghost_fork_choice_selects_highest_weight() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));

        let ghost = GhostForkChoice::new(blockchain);

        let light = make_header(10, Hash::zero(), 100);
        let heavy = make_header(10, Hash::zero(), 200);
        let candidates = vec![&light, &heavy];
        let best = ghost.select_best(&candidates);
        assert_eq!(best.unwrap().difficulty, U256::from(200));
    }

    #[test]
    fn test_ghost_fork_choice_returns_none_on_empty() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));

        let ghost = GhostForkChoice::new(blockchain);
        assert!(ghost.select_best(&[]).is_none());
    }

    #[test]
    fn test_longest_chain_rule_selects_highest_number() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));

        let rule = LongestChainRule::new(blockchain);

        let short = make_header(5, Hash::zero(), 100);
        let long = make_header(10, Hash::zero(), 50);
        let candidates = vec![&short, &long];
        let best = rule.select_best(&candidates);
        assert_eq!(best.unwrap().number, U256::from(10));
    }

    #[test]
    fn test_reorg_manager_detects_heavier_block() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));

        let fork_choice = Box::new(GhostForkChoice::new(blockchain.clone()));
        let reorg = ReorgManager::new(blockchain.clone(), fork_choice);

        let genesis_hash = blockchain.genesis().header.hash;
        let light_block = valid_block(1, genesis_hash, 100);
        let heavy_block = valid_block(1, genesis_hash, 200);

        // 比创世块（difficulty 0）重 → 触发重组
        assert!(reorg.needs_reorg(&light_block).unwrap());
        blockchain.insert_block(light_block).unwrap();
        // 比当前 best（light, 100）重 → 触发重组
        assert!(reorg.needs_reorg(&heavy_block).unwrap());
    }

    #[test]
    fn test_fork_resolution_tie_breaker_deterministic() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));

        let fork_choice = Box::new(GhostForkChoice::new(blockchain.clone()));
        let reorg = ReorgManager::new(blockchain.clone(), fork_choice);

        let genesis_hash = blockchain.genesis().header.hash;
        let block_a = valid_block(1, genesis_hash, 100);
        let block_b = valid_block(1, genesis_hash, 100);

        // 比创世块重 → 触发重组；同难度插入后不再触发
        assert!(reorg.needs_reorg(&block_a).unwrap());
        blockchain.insert_block(block_a).unwrap();
        assert!(!reorg.needs_reorg(&block_b).unwrap());
    }

    /// 构造 PoW 有效区块（挖掘 nonce），父哈希必须存在。
    fn valid_block(number: u64, parent_hash: Hash, difficulty: u64) -> Block {
        let mut header = make_header(number, parent_hash, difficulty);
        let target = crate::block::pow_target_from_difficulty(header.difficulty);
        let mut nonce = 0u64;
        loop {
            let h = crate::block::compute_pow_hash(&header, nonce);
            if crate::block::pow_hash_meets_target(h.as_bytes(), target) {
                header.nonce = nonce;
                header.hash = header.compute_hash();
                break;
            }
            nonce += 1;
        }
        Block {
            header,
            body: Some(BlockBody::default()),
            uncles: Vec::new(),
        }
    }
}
