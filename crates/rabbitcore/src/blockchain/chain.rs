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
    /// State executor（结算进共识：区块体内计算交易的执行与校验）
    state_executor: Arc<crate::state::executor::StateExecutor>,
}

impl Blockchain {
    /// Create new blockchain
    pub fn new(consensus: Arc<PowConsensus>, state_db: Arc<StateDb>) -> Self {
        let executor = Arc::new(crate::state::executor::StateExecutor::new(
            state_db.clone(),
            0,
        ));
        Self::new_with_executor(consensus, state_db, executor)
    }

    /// 带共享状态执行器的构造（BlockTime 迁移时接入节点共享 compute store）。
    pub fn new_with_executor(
        consensus: Arc<PowConsensus>,
        state_db: Arc<StateDb>,
        state_executor: Arc<crate::state::executor::StateExecutor>,
    ) -> Self {
        let genesis = create_genesis_block();
        let genesis_hash = genesis.header.hash;

        let mut blocks = HashMap::new();
        blocks.insert(genesis_hash, genesis.clone());

        let mut hashes_by_number = HashMap::new();
        hashes_by_number.insert(0, genesis_hash);

        Self {
            genesis: genesis.clone(),
            best_block: RwLock::new(genesis.clone()),
            blocks: RwLock::new(blocks),
            hashes_by_number: RwLock::new(hashes_by_number),
            total_difficulty: RwLock::new(genesis.header.difficulty),
            consensus,
            state_db,
            state_executor,
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

        // 结算进共识：区块含计算交易时执行并校验（receipts 一致性 + state_root 承诺）。
        // 空区块（现有链）跳过，保持兼容；BlockTime 迁移后所有区块都走此路径。
        let has_compute_txs = !block
            .body
            .as_ref()
            .map(|b| b.transactions.is_empty())
            .unwrap_or(true);
        if has_compute_txs {
            let transition = self
                .state_executor
                .execute_block(&block, parent.header.state_root)
                .map_err(|e| BlockchainError::InvalidBlock(e.to_string()))?;
            if transition.to_root != block.header.state_root {
                return Err(BlockchainError::InvalidStateRoot);
            }
        }

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
    use crate::block::{BlockBody, Receipt, ReceiptStatus};
    use crate::compute::{
        Command, ComputeTx, ObjectId, ObjectKind, OutputId, OutputProposal, Script, TxId,
        TxWitness, Version,
    };
    use crate::crypto::Address;

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

    /// 构造含 1 笔 Mint 计算交易 + 正确 receipts 的区块（difficulty=1 → PoW 恒过）。
    fn compute_block(receipts: Vec<Receipt>) -> Block {
        use crate::compute::{
            ObjectId, ObjectKind, OutputId, OutputProposal, Script, TxId, TxSignature, TxWitness,
            Version,
        };
        let consensus = PowConsensus::new(PowAlgorithm::LightHash);
        let genesis = create_genesis_block();

        let object_id = ObjectId(Hash::from_bytes(crate::crypto::keccak256(b"chain-test-obj")));
        let output = OutputProposal {
            output_id: OutputId(Hash::from_bytes(crate::crypto::keccak256(b"chain-test-out"))),
            object_id,
            domain_id: crate::compute::GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: crate::compute::Ownership::Shared,
            predecessor: None,
            version: Version(1),
            state: b"session".to_vec(),
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
        };
        let mut tx = ComputeTx {
            tx_id: TxId(Hash::zero()),
            domain_id: crate::compute::GAME_DOMAIN,
            command: Command::Mint,
            input_set: vec![],
            read_set: vec![],
            output_proposals: vec![output],
            fee: 0,
            nonce: Some(1),
            metadata: vec![],
            payload: vec![],
            deadline_unix_secs: None,
            chain_id: Some(10088),
            network_id: Some(10088),
            witness: TxWitness { signatures: vec![], threshold: None },
            max_fee: 0,
            priority_fee: 0,
            gas_limit: 0,
        };
        // 阈值默认 1：Mint 也需至少 1 个签名（witness 签名不进入 tx_id preimage）
        use ed25519_dalek::Signer as _;
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
        let signature = key.sign(&tx.signing_preimage()).to_bytes();
        let public_key = key.verifying_key().to_bytes();
        tx.witness.signatures = vec![TxSignature::ed25519(signature, public_key)];
        let tx = tx.with_expected_tx_id();

        let mut header = BlockHeader {
            version: 3,
            parent_hash: genesis.header.hash,
            uncle_hashes: vec![],
            coinbase: Address::zero(),
            state_root: Hash::zero(), // 与 chain StateDb 初始根一致（mint 不触碰账户状态）
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(1),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_700_000_000,
            difficulty: U256::from(1),
            nonce: 0,
            extra_data: vec![],
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000),
            hash: Hash::zero(),
        };
        let body = BlockBody {
            version: 1,
            transactions: vec![tx],
            receipts,
        };
        header.apply_body_commitments(&body);
        let mut block = Block::new_with_body(header, body);
        for r in block.body.as_mut().unwrap().receipts.iter_mut() {
            r.block_hash = block.header.hash;
        }
        let _ = consensus;
        block
    }

    #[test]
    fn compute_block_with_matching_receipts_inserts() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Blockchain::new(consensus, state_db);

        let block = compute_block(vec![]); // receipts 需匹配；先不插，直接验证路径
        let txs = block.body.as_ref().unwrap().transactions.clone();
        let receipts = txs
            .iter()
            .map(|tx| Receipt {
                tx_id: tx.tx_id,
                block_hash: Hash::zero(),
                status: ReceiptStatus::Success,
                gas_used: crate::compute::estimate_tx_gas(tx),
                compute_units: 0,
                output_refs: tx.output_proposals.iter().map(|p| p.output_id).collect(),
                logs: vec![],
                error: None,
            })
            .collect();
        let block = compute_block(receipts);

        let inserted = blockchain.insert_block(block).expect("compute block accepted");
        assert!(inserted);
    }

    #[test]
    fn compute_block_with_tampered_receipt_rejected() {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Blockchain::new(consensus, state_db);

        let block = compute_block(vec![Receipt {
            tx_id: crate::compute::TxId(Hash::from_bytes([9; 32])),
            block_hash: Hash::zero(),
            status: ReceiptStatus::Failed,
            gas_used: 0,
            compute_units: 0,
            output_refs: vec![],
            logs: vec![],
            error: Some("forged".to_string()),
        }]);

        let err = blockchain
            .insert_block(block)
            .expect_err("tampered receipt must be rejected");
        assert!(err.to_string().contains("receipt mismatch") || err.to_string().contains("tx_id"));
    }
}
