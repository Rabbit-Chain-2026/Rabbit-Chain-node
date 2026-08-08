//! Consensus module - PoW implementation

use crate::account::U256;
use crate::block::{adjust_mining_difficulty, Block, BlockHeader};
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
        // RabbitChain PoW is LightHash: Keccak256 over the RABBIT-POW-V1
        // domain-separated canonical preimage. RandomX/ProgPoW are NOT
        // implemented; configuring them maps to the same LightHash proof so
        // consensus and mining never diverge.
        crate::block::compute_pow_hash(header, nonce)
    }

    fn difficulty_to_target(&self, difficulty: U256) -> U256 {
        if difficulty.is_zero() {
            return U256::from_big_endian(&[0xFFu8; 32]);
        }
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
        let actual_block_time = current_timestamp.saturating_sub(parent.timestamp).max(1) as u128;
        let bootstrap_threshold = self.min_difficulty.as_u128().saturating_mul(10).max(1);
        adjust_mining_difficulty(
            parent.difficulty,
            actual_block_time,
            self.target_block_time,
            self.min_difficulty.as_u128(),
            self.max_difficulty.as_u128(),
            bootstrap_threshold,
        )
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
        // Keep consensus path aligned with BlockHeader::verify_pow:
        // genesis is exempt; non-genesis zero difficulty is invalid.
        if header.number.is_zero() {
            return Ok(());
        }
        if header.difficulty.is_zero() {
            return Err(ConsensusError::InvalidDifficulty);
        }
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
        let mut parent = create_genesis_block().header;
        parent.timestamp = 100;
        parent.difficulty = U256::from_u128(2_000_000);

        // Fast block should raise difficulty
        let new_diff = consensus.calculate_difficulty(&parent, 105);
        assert_eq!(new_diff, U256::from_u128(4_000_000));

        // Slow block should lower difficulty, but never below the configured minimum.
        let slower = consensus.calculate_difficulty(&parent, 120);
        assert_eq!(slower, U256::from_u128(1_000_000));
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

    #[test]
    fn test_verify_pow_rejects_unmet_target() {
        let consensus = PowConsensus::new(PowAlgorithm::ProgPoW);
        let mut header = create_genesis_block().header;
        // Non-genesis so zero-difficulty exemption does not apply.
        header.number = U256::from(1u64);
        header.difficulty = U256::from_u128(u128::MAX);
        let result = consensus.verify_pow(&header);
        assert!(result.is_err());
    }

    #[test]
    fn test_calculate_reward_initial_value() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let reward = consensus.calculate_reward(U256::zero());
        assert_eq!(reward, U256::from_u128(5_000_000_000_000_000_000u128));
    }

    #[test]
    fn test_calculate_reward_after_multiple_halvings() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let reward_1 = consensus.calculate_reward(U256::from(2_100_000));
        let reward_2 = consensus.calculate_reward(U256::from(4_200_000));
        let reward_3 = consensus.calculate_reward(U256::from(6_300_000));
        assert_eq!(reward_1.as_u128() / 2, reward_2.as_u128());
        assert_eq!(reward_2.as_u128() / 2, reward_3.as_u128());
    }

    #[test]
    fn test_validate_block_rejects_missing_parent() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let genesis = create_genesis_block();
        let mut orphan = genesis.clone();
        orphan.header.parent_hash = Hash::from_bytes([0xff; 32]);
        let result = consensus.validate_block(&orphan, &genesis.header);
        assert!(result.is_err());
    }

    #[test]
    fn test_difficulty_monotonic_increases_for_fast_blocks() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let mut parent = create_genesis_block().header;
        parent.timestamp = 100;
        parent.difficulty = U256::from_u128(1_000_000);

        let difficulty = consensus.calculate_difficulty(&parent, 101);
        assert!(difficulty > parent.difficulty);
    }

    #[test]
    fn test_difficulty_monotonic_decreases_for_slow_blocks() {
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let mut parent = create_genesis_block().header;
        parent.timestamp = 100;
        parent.difficulty = U256::from_u128(5_000_000);

        let difficulty = consensus.calculate_difficulty(&parent, 200);
        assert!(difficulty < parent.difficulty);
    }
}
