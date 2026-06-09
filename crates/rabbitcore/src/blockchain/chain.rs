//! Blockchain chain management

use super::{BlockchainError, Result};
use crate::account::U256;
use crate::block::{create_genesis_block, Block, BlockHeader};
use crate::consensus::{Consensus, PowAlgorithm, PowConsensus};
use crate::crypto::Hash;
use crate::state::StateDb;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Chain information
#[derive(Clone, Debug)]
pub struct ChainInfo {
    /// Genesis hash
    pub genesis_hash: Hash,
    /// Best block hash
    pub best_hash: Hash,
    /// Best block number
    pub best_number: u64,
    /// Total difficulty
    pub total_difficulty: U256,
}

/// Blockchain
pub struct Blockchain {
    /// Genesis block
    genesis: Block,
    /// Best block
    best_block: RwLock<Block>,
    /// Block storage
    blocks: RwLock<HashMap<Hash, Block>>,
    /// Block hashes by number
    hashes_by_number: RwLock<HashMap<u64, Hash>>,
    /// Total difficulty
    total_difficulty: RwLock<U256>,
    /// Consensus
    consensus: Arc<PowConsensus>,
    /// State database
    state_db: Arc<StateDb>,
}

impl Blockchain {
    /// Create new blockchain
    pub fn new(consensus: Arc<PowConsensus>, state_db: Arc<StateDb>) -> Self {
        let genesis = create_genesis_block();
        let genesis_hash = genesis.header.hash;

        let mut blocks = HashMap::new();
        blocks.insert(genesis_hash, genesis.clone());

        let mut hashes_by_number = HashMap::new();
        hashes_by_number.insert(0, genesis_hash);

        Self {
            genesis,
            best_block: RwLock::new(genesis),
            blocks: RwLock::new(blocks),
            hashes_by_number: RwLock::new(hashes_by_number),
            total_difficulty: RwLock::new(genesis.header.difficulty),
            consensus,
            state_db,
        }
    }

    /// Get genesis block
    pub fn genesis(&self) -> &Block {
        &self.genesis
    }

    /// Get best block
    pub fn best_block(&self) -> Block {
        self.best_block.read().clone()
    }

    /// Get best block number
    pub fn best_number(&self) -> u64 {
        self.best_block.read().header.number.as_u64()
    }

    /// Get best block hash
    pub fn best_hash(&self) -> Hash {
        self.best_block.read().header.hash
    }

    /// Get block by hash
    pub fn get_block(&self, hash: &Hash) -> Option<Block> {
        self.blocks.read().get(hash).cloned()
    }

    /// Get block by number
    pub fn get_block_by_number(&self, number: u64) -> Option<Block> {
        self.hashes_by_number
            .read()
            .get(&number)
            .and_then(|hash| self.get_block(hash))
    }

    /// Get block hash by number
    pub fn get_block_hash(&self, number: u64) -> Option<Hash> {
        self.hashes_by_number.read().get(&number).copied()
    }

    /// Insert block
    pub fn insert_block(&self, block: Block) -> Result<bool> {
        let hash = block.header.hash;

        // Check if already exists
        if self.blocks.read().contains_key(&hash) {
            return Ok(false);
        }

        // Validate block
        self.validate_block(&block)?;

        // Check if parent exists
        let parent = self
            .get_block(&block.header.parent_hash)
            .ok_or(BlockchainError::OrphanBlock)?;

        // Calculate total difficulty
        let parent_td = self.get_total_difficulty(&parent.header.hash)?;
        let total_difficulty = parent_td + block.header.difficulty;

        // Store block
        self.blocks.write().insert(hash, block.clone());
        self.hashes_by_number
            .write()
            .insert(block.header.number.as_u64(), hash);

        // Update best block if this chain has more difficulty
        let current_td = *self.total_difficulty.read();

        if total_difficulty > current_td {
            self.update_best_block(block, total_difficulty)?;
            Ok(true) // New best block
        } else {
            Ok(false) // Side chain
        }
    }

    /// Validate block
    fn validate_block(&self, block: &Block) -> Result<()> {
        // Get parent
        let parent = self
            .get_block(&block.header.parent_hash)
            .ok_or_else(|| BlockchainError::InvalidBlock("Parent not found".into()))?;

        // Validate header
        block
            .header
            .validate(&parent.header)
            .map_err(|e| BlockchainError::InvalidBlock(e.to_string()))?;

        // Validate PoW
        self.consensus
            .verify_pow(&block.header)
            .map_err(|e| BlockchainError::Consensus(e.to_string()))?;

        // Validate state root (would execute transactions)
        // Simplified for now

        Ok(())
    }

    /// Update best block
    fn update_best_block(&self, block: Block, total_difficulty: U256) -> Result<()> {
        // Apply state transitions
        self.apply_block_state(&block)?;

        // Update best block
        *self.best_block.write() = block;
        *self.total_difficulty.write() = total_difficulty;

        Ok(())
    }

    /// Apply block state transitions
    fn apply_block_state(&self, block: &Block) -> Result<()> {
        // Would execute transactions and update state
        // Simplified for now

        Ok(())
    }

    /// Get total difficulty for block
    fn get_total_difficulty(&self, hash: &Hash) -> Result<U256> {
        // Would calculate from genesis
        // Simplified
        Ok(self.consensus.calculate_reward(U256::from(1000)))
    }

    /// Get chain info
    pub fn get_chain_info(&self) -> ChainInfo {
        ChainInfo {
            genesis_hash: self.genesis.header.hash,
            best_hash: self.best_hash(),
            best_number: self.best_number(),
            total_difficulty: *self.total_difficulty.read(),
        }
    }

    /// Get blocks to sync
    pub fn get_sync_headers(&self, from_number: u64, limit: u64) -> Vec<BlockHeader> {
        let mut headers = Vec::new();

        for i in 0..limit {
            let number = from_number + i;
            if let Some(block) = self.get_block_by_number(number) {
                headers.push(block.header);
            } else {
                break;
            }
        }

        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blockchain_creation() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));

        let blockchain = Blockchain::new(consensus, state_db);

        assert_eq!(blockchain.best_number(), 0);
        assert!(!blockchain.best_hash().is_zero());
    }

    #[test]
    fn test_get_block() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));

        let blockchain = Blockchain::new(consensus, state_db);

        let genesis = blockchain.genesis();
        let retrieved = blockchain.get_block(&genesis.header.hash).unwrap();

        assert_eq!(retrieved.header.number, U256::zero());
    }
}
