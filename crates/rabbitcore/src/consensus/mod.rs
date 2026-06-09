//! Consensus module - PoW implementation

use crate::account::U256;
use crate::block::{Block, BlockHeader};
use crate::crypto::Hash;
use thiserror::Error;

/// Consensus errors
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ConsensusError {
    #[error("Invalid block")]
    InvalidBlock,
    #[error("Invalid PoW")]
    InvalidPow,
    #[error("Invalid difficulty")]
    InvalidDifficulty,
    #[error("Block already exists")]
    BlockExists,
    #[error("Orphan block")]
    OrphanBlock,
    #[error("Invalid state transition")]
    InvalidStateTransition,
}

/// PoW algorithm type
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowAlgorithm {
    RandomX,
    ProgPoW,
    LightHash,
}

/// Consensus trait
pub trait Consensus: Send + Sync {
    fn validate_block(&self, block: &Block, parent: &BlockHeader) -> Result<(), ConsensusError>;
    fn calculate_difficulty(&self, parent: &BlockHeader, timestamp: u64) -> U256;
    fn calculate_reward(&self, block_number: U256) -> U256;
    fn verify_pow(&self, header: &BlockHeader) -> Result<(), ConsensusError>;
}

/// PoW consensus engine
pub struct PowConsensus {
    algorithm: PowAlgorithm,
    target_block_time: u64,
    min_difficulty: U256,
    max_difficulty: U256,
    initial_reward: U256,
    halving_period: u64,
}

impl PowConsensus {
    pub fn new(algorithm: PowAlgorithm) -> Self {
        Self {
            algorithm,
            target_block_time: 10,
            min_difficulty: U256::from_u128(1_000_000),
            max_difficulty: U256::from_u128(u128::MAX),
            initial_reward: U256::from_u128(5_000_000_000_000_000_000u128),
            halving_period: 2_100_000,
        }
    }

    pub fn compute(&self, header: &BlockHeader, nonce: u64) -> Hash {
        match self.algorithm {
            PowAlgorithm::RandomX => self.compute_randomx(header, nonce),
            PowAlgorithm::ProgPoW => self.compute_progpow(header, nonce),
            PowAlgorithm::LightHash => self.compute_lighthash(header, nonce),
        }
    }

    fn compute_randomx(&self, header: &BlockHeader, nonce: u64) -> Hash {
        let mut data = Vec::new();
        data.extend_from_slice(&header.number.to_big_endian());
        data.extend_from_slice(&nonce.to_be_bytes());
        Hash::from_bytes(crate::crypto::blake3_hash(&data))
    }

    fn compute_progpow(&self, header: &BlockHeader, nonce: u64) -> Hash {
        let mut data = header.hash.as_bytes().to_vec();
        data.extend_from_slice(&nonce.to_be_bytes());
        Hash::from_bytes(crate::crypto::keccak256(&data))
    }

    fn compute_lighthash(&self, header: &BlockHeader, nonce: u64) -> Hash {
        let mut data = Vec::new();
        data.extend_from_slice(&header.number.to_big_endian());
        data.extend_from_slice(&nonce.to_be_bytes());
        Hash::from_bytes(crate::crypto::keccak256(&data))
    }

    fn difficulty_to_target(&self, difficulty: U256) -> U256 {
        U256::from_big_endian(&[0xFFu8; 32]) / difficulty
    }
}

impl Consensus for PowConsensus {
    fn validate_block(&self, block: &Block, parent: &BlockHeader) -> Result<(), ConsensusError> {
        block
            .header
            .validate(parent)
            .map_err(|_| ConsensusError::InvalidBlock)?;
        self.verify_pow(&block.header)?;
        Ok(())
    }

    fn calculate_difficulty(&self, parent: &BlockHeader, current_timestamp: u64) -> U256 {
        let actual_block_time = current_timestamp.saturating_sub(parent.timestamp);

        // Adjust difficulty based on block time
        // Fast block (< target): increase difficulty
        // Slow block (> target): decrease difficulty
        let difficulty_delta = if actual_block_time < self.target_block_time {
            // Fast block - increase difficulty
            let ratio = self.target_block_time as f64 / actual_block_time.max(1) as f64;
            ratio.min(1.1)
        } else {
            // Slow block - decrease difficulty
            let ratio = self.target_block_time as f64 / actual_block_time as f64;
            ratio.max(0.9)
        };

        let parent_diff = parent.difficulty.as_u128() as f64;
        let mut new_difficulty = (parent_diff * difficulty_delta) as u128;

        new_difficulty = new_difficulty.max(self.min_difficulty.as_u128());
        new_difficulty = new_difficulty.min(self.max_difficulty.as_u128());

        U256::from_u128(new_difficulty)
    }

    fn calculate_reward(&self, block_number: U256) -> U256 {
        let halving_count = block_number.as_u64() / self.halving_period;

        let mut reward = self.initial_reward;
        for _ in 0..halving_count {
            reward = U256::from_u128(reward.as_u128() / 2);
        }

        reward
    }

    fn verify_pow(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        let target = self.difficulty_to_target(header.difficulty);
        let pow_hash = self.compute(header, header.nonce);

        // Compare hash with target
        let hash_value = U256::from_big_endian(pow_hash.as_bytes());

        if hash_value > target {
            return Err(ConsensusError::InvalidPow);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::create_genesis_block;

    #[test]
    fn test_difficulty_adjustment() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let genesis = create_genesis_block();

        // Fast block (5 seconds instead of 10)
        let new_diff = consensus.calculate_difficulty(&genesis.header, 5);
        assert!(new_diff > genesis.header.difficulty);

        // Slow block (20 seconds instead of 10)
        let new_diff = consensus.calculate_difficulty(&genesis.header, 20);
        assert!(new_diff < genesis.header.difficulty);
    }

    #[test]
    fn test_block_reward_halving() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);

        let reward_0 = consensus.calculate_reward(U256::zero());
        let reward_after_halving = consensus.calculate_reward(U256::from(2_100_000));
        let reward_after_many_halvings = consensus.calculate_reward(U256::from(2_100_000_u64 * 8));

        assert_eq!(reward_after_halving.as_u128(), reward_0.as_u128() / 2);
        assert!(reward_after_many_halvings < reward_after_halving);
    }
}
