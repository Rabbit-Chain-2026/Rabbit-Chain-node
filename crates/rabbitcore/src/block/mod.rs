//! Block module

use crate::account::U256;
use crate::compute::{ComputeTx, OutputId, TxId};
use crate::crypto::{Address, Hash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PRE_CANONICAL_BLOCK_VERSION: u32 = 2;
pub const CANONICAL_BLOCK_VERSION: u32 = 3;

/// Block errors
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    #[error("Invalid parent hash")]
    InvalidParentHash,
    #[error("Invalid block number")]
    InvalidBlockNumber,
    #[error("Invalid timestamp")]
    InvalidTimestamp,
    #[error("Invalid difficulty")]
    InvalidDifficulty,
    #[error("Invalid PoW")]
    InvalidPow,
    #[error("Gas limit too high")]
    GasLimitTooHigh,
    #[error("Extra data too large")]
    ExtraDataTooLarge,
    #[error("Invalid transaction root")]
    InvalidTransactionRoot,
    #[error("Invalid receipts root")]
    InvalidReceiptsRoot,
    #[error("Invalid state root")]
    InvalidStateRoot,
    #[error("Invalid block body")]
    InvalidBlockBody,
    #[error("Block too large")]
    BlockTooLarge,
}

/// Block header
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockHeader {
    pub version: u32,
    pub parent_hash: Hash,
    pub uncle_hashes: Vec<Hash>,
    pub coinbase: Address,
    pub state_root: Hash,
    pub transactions_root: Hash,
    pub receipts_root: Hash,
    pub number: U256,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub timestamp: u64,
    pub difficulty: U256,
    pub nonce: u64,
    pub extra_data: Vec<u8>,
    pub mix_hash: Hash,
    pub base_fee_per_gas: U256,
    #[serde(skip)]
    pub hash: Hash,
}

impl BlockHeader {
    pub fn compute_hash(&self) -> Hash {
        let encoded = if self.version >= CANONICAL_BLOCK_VERSION {
            self.encode_canonical_hash_preimage()
        } else {
            self.encode_legacy_hash_preimage()
        };
        Hash::from_bytes(crate::crypto::keccak256(&encoded))
    }

    pub fn validate(&self, parent: &BlockHeader) -> Result<(), BlockError> {
        if self.parent_hash != parent.hash {
            return Err(BlockError::InvalidParentHash);
        }

        if self.number != parent.number + U256::one() {
            return Err(BlockError::InvalidBlockNumber);
        }

        if self.timestamp <= parent.timestamp {
            return Err(BlockError::InvalidTimestamp);
        }

        if self.extra_data.len() > 32 {
            return Err(BlockError::ExtraDataTooLarge);
        }

        Ok(())
    }

    pub fn verify_pow(&self) -> Result<(), BlockError> {
        let target = pow_target_from_difficulty(self.difficulty);
        let pow_hash = compute_pow_hash(self, self.nonce);

        if !pow_hash_meets_target(pow_hash.as_bytes(), target) {
            return Err(BlockError::InvalidPow);
        }

        Ok(())
    }

    pub fn apply_body_commitments(&mut self, body: &BlockBody) {
        let roots = body.commitment_roots();
        self.transactions_root = roots.transactions_root;
        self.receipts_root = roots.receipts_root;
    }

    pub fn reconcile_body_commitments(&mut self, body: &BlockBody) -> Result<(), BlockError> {
        let roots = body.commitment_roots();
        if !self.transactions_root.is_zero() && self.transactions_root != roots.transactions_root {
            return Err(BlockError::InvalidTransactionRoot);
        }
        if !self.receipts_root.is_zero() && self.receipts_root != roots.receipts_root {
            return Err(BlockError::InvalidReceiptsRoot);
        }
        self.apply_body_commitments(body);
        Ok(())
    }

    pub fn validate_body_commitments(&self, body: &BlockBody) -> Result<(), BlockError> {
        body.validate_against_header(self)
    }

    fn encode_legacy_hash_preimage(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.version.to_be_bytes());
        data.extend_from_slice(self.parent_hash.as_bytes());
        data.extend_from_slice(&self.number.to_big_endian());
        data.extend_from_slice(&self.timestamp.to_be_bytes());
        data.extend_from_slice(&self.nonce.to_be_bytes());
        data.extend_from_slice(&self.difficulty.to_big_endian());
        data
    }

    fn encode_canonical_hash_preimage(&self) -> Vec<u8> {
        bincode::serialize(&CanonicalBlockHashPreimage {
            version: self.version,
            parent_hash: self.parent_hash,
            uncle_hashes: &self.uncle_hashes,
            coinbase: self.coinbase,
            state_root: self.state_root,
            transactions_root: self.transactions_root,
            receipts_root: self.receipts_root,
            number: self.number,
            gas_limit: self.gas_limit,
            gas_used: self.gas_used,
            timestamp: self.timestamp,
            difficulty: self.difficulty,
            nonce: self.nonce,
            extra_data: &self.extra_data,
            mix_hash: self.mix_hash,
            base_fee_per_gas: self.base_fee_per_gas,
        })
        .expect("serializing canonical block hash preimage")
    }
}

/// Complete block
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<BlockBody>,
    pub uncles: Vec<BlockHeader>,
}

impl Block {
    pub fn new(header: BlockHeader) -> Self {
        Self {
            header,
            body: Some(BlockBody::default()),
            uncles: Vec::new(),
        }
    }

    pub fn new_with_body(mut header: BlockHeader, body: BlockBody) -> Self {
        header.apply_body_commitments(&body);
        header.hash = header.compute_hash();
        Self {
            header,
            body: Some(body),
            uncles: Vec::new(),
        }
    }

    pub fn with_body(mut self, body: BlockBody) -> Self {
        self.body = Some(body);
        self
    }

    pub fn body(&self) -> Option<&BlockBody> {
        self.body.as_ref()
    }

    pub fn body_mut(&mut self) -> Option<&mut BlockBody> {
        self.body.as_mut()
    }

    pub fn set_body(&mut self, body: BlockBody) {
        self.body = Some(body);
    }

    pub fn take_body(&mut self) -> Option<BlockBody> {
        self.body.take()
    }

    pub fn encode_rlp(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.header.number.to_big_endian());
        data
    }
}

/// Canonical transaction envelope stored in a block body.
///
/// P0 reuses the existing compute transaction shape as the canonical payload so
/// the sidecar path can be introduced without changing transaction encoding.
pub type TxEnvelope = ComputeTx;

#[derive(Serialize)]
struct CanonicalBlockHashPreimage<'a> {
    version: u32,
    parent_hash: Hash,
    uncle_hashes: &'a [Hash],
    coinbase: Address,
    state_root: Hash,
    transactions_root: Hash,
    receipts_root: Hash,
    number: U256,
    gas_limit: u64,
    gas_used: u64,
    timestamp: u64,
    difficulty: U256,
    nonce: u64,
    extra_data: &'a [u8],
    mix_hash: Hash,
    base_fee_per_gas: U256,
}

/// Canonical execution receipt stored alongside a block body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_id: TxId,
    pub block_hash: Hash,
    pub status: ReceiptStatus,
    pub gas_used: u64,
    pub compute_units: u64,
    pub output_refs: Vec<OutputId>,
    pub logs: Vec<ReceiptLog>,
    pub error: Option<String>,
}

impl Receipt {
    pub fn success(
        tx_id: TxId,
        block_hash: Hash,
        gas_used: u64,
        compute_units: u64,
        output_refs: Vec<OutputId>,
    ) -> Self {
        Self {
            tx_id,
            block_hash,
            status: ReceiptStatus::Success,
            gas_used,
            compute_units,
            output_refs,
            logs: Vec::new(),
            error: None,
        }
    }

    pub fn reverted(tx_id: TxId, block_hash: Hash, error: impl Into<String>) -> Self {
        Self {
            tx_id,
            block_hash,
            status: ReceiptStatus::Reverted,
            gas_used: 0,
            compute_units: 0,
            output_refs: Vec::new(),
            logs: Vec::new(),
            error: Some(error.into()),
        }
    }

    pub fn failed(tx_id: TxId, block_hash: Hash, error: impl Into<String>) -> Self {
        Self {
            tx_id,
            block_hash,
            status: ReceiptStatus::Failed,
            gas_used: 0,
            compute_units: 0,
            output_refs: Vec::new(),
            logs: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// Receipt execution status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptStatus {
    Success,
    Reverted,
    Failed,
}

/// Structured receipt log item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptLog {
    pub topic: String,
    pub data: Vec<u8>,
}

/// Block body commitment roots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockBodyRoots {
    pub transactions_root: Hash,
    pub receipts_root: Hash,
}

/// Canonical body payload stored alongside a header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockBody {
    #[serde(default = "BlockBody::default_version")]
    pub version: u32,
    pub transactions: Vec<TxEnvelope>,
    pub receipts: Vec<Receipt>,
}

impl BlockBody {
    pub const fn default_version() -> u32 {
        1
    }

    pub fn empty() -> Self {
        Self {
            version: Self::default_version(),
            transactions: Vec::new(),
            receipts: Vec::new(),
        }
    }

    pub fn new(transactions: Vec<TxEnvelope>, receipts: Vec<Receipt>) -> Self {
        Self {
            version: Self::default_version(),
            transactions,
            receipts,
        }
    }

    pub fn tx_count(&self) -> usize {
        self.transactions.len()
    }

    pub fn receipt_count(&self) -> usize {
        self.receipts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty() && self.receipts.is_empty()
    }

    pub fn commitment_roots(&self) -> BlockBodyRoots {
        BlockBodyRoots {
            transactions_root: compute_transactions_root(&self.transactions),
            receipts_root: compute_receipts_root(&self.receipts),
        }
    }

    pub fn validate_against_header(&self, header: &BlockHeader) -> Result<(), BlockError> {
        if self.transactions.len() != self.receipts.len() {
            return Err(BlockError::InvalidBlockBody);
        }

        for (tx, receipt) in self.transactions.iter().zip(self.receipts.iter()) {
            if tx.tx_id != receipt.tx_id || receipt.block_hash != header.hash {
                return Err(BlockError::InvalidBlockBody);
            }
        }

        let roots = self.commitment_roots();
        if header.transactions_root != roots.transactions_root {
            return Err(BlockError::InvalidTransactionRoot);
        }
        if header.receipts_root != roots.receipts_root {
            return Err(BlockError::InvalidReceiptsRoot);
        }

        Ok(())
    }
}

impl Default for BlockBody {
    fn default() -> Self {
        Self::empty()
    }
}

/// Canonical block plus body sidecar record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockBodyRecord {
    pub number: u64,
    pub block_hash: Hash,
    pub body: BlockBody,
}

impl BlockBodyRecord {
    pub fn new(number: u64, block_hash: Hash, body: BlockBody) -> Self {
        Self {
            number,
            block_hash,
            body,
        }
    }
}

/// Compute the canonical Merkle root for an ordered list of leaf hashes.
pub fn compute_merkle_root(hashes: &[Hash]) -> Hash {
    if hashes.is_empty() {
        return Hash::from_bytes([0u8; 32]);
    }

    if hashes.len() == 1 {
        return hashes[0];
    }

    let mut level: Vec<Hash> = hashes.to_vec();

    while level.len() > 1 {
        let mut next_level = Vec::with_capacity(level.len().div_ceil(2));

        for i in (0..level.len()).step_by(2) {
            let left = level[i];
            let right = if i + 1 < level.len() {
                level[i + 1]
            } else {
                level[i]
            };

            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(left.as_bytes());
            data.extend_from_slice(right.as_bytes());
            next_level.push(Hash::from_bytes(crate::crypto::keccak256(&data)));
        }

        level = next_level;
    }

    level[0]
}

/// Compute the transaction commitment root for a block body.
pub fn compute_transactions_root(transactions: &[TxEnvelope]) -> Hash {
    let leaves = transactions
        .iter()
        .map(|tx| commitment_leaf_hash(b"RABBIT-BLOCK-TX-V1", tx))
        .collect::<Vec<_>>();
    compute_merkle_root(&leaves)
}

/// Compute the receipt commitment root for a block body.
pub fn compute_receipts_root(receipts: &[Receipt]) -> Hash {
    let leaves = receipts
        .iter()
        .map(|receipt| commitment_leaf_hash(b"RABBIT-BLOCK-RECEIPT-V1", receipt))
        .collect::<Vec<_>>();
    compute_merkle_root(&leaves)
}

fn commitment_leaf_hash<T: Serialize>(domain: &[u8], value: &T) -> Hash {
    let serialized = bincode::serialize(value).expect("serializing block commitment payload");
    let mut data = Vec::with_capacity(domain.len() + serialized.len());
    data.extend_from_slice(domain);
    data.extend_from_slice(&serialized);
    Hash::from_bytes(crate::crypto::keccak256(&data))
}

/// Genesis block
pub fn create_genesis_block() -> Block {
    let header = BlockHeader {
        version: 1,
        parent_hash: Hash::zero(),
        uncle_hashes: Vec::new(),
        coinbase: Address::zero(),
        state_root: Hash::from_bytes([0u8; 32]),
        transactions_root: Hash::from_bytes([0u8; 32]),
        receipts_root: Hash::from_bytes([0u8; 32]),
        number: U256::zero(),
        gas_limit: 30_000_000,
        gas_used: 0,
        timestamp: 0,
        difficulty: U256::from_u128(1_000_000_000_000_000u128),
        nonce: 0,
        extra_data: b"RabbitChain Genesis".to_vec(),
        mix_hash: Hash::zero(),
        base_fee_per_gas: U256::from(1_000_000_000),
        hash: Hash::zero(),
    };

    let hash = header.compute_hash();
    let mut header = header;
    header.hash = hash;
    Block::new(header).with_body(BlockBody::default())
}

pub fn max_pow_target() -> U256 {
    U256::from_big_endian(&[0xFFu8; 32])
}

pub fn pow_target_from_difficulty(difficulty: U256) -> U256 {
    if difficulty.is_zero() {
        return max_pow_target();
    }
    let divisor = difficulty.as_u128();
    if divisor == 0 {
        return U256::zero();
    }
    if divisor > (u128::MAX >> 8) {
        return U256::zero();
    }

    let mut quotient = [0u8; 32];
    let mut remainder = 0u128;
    for slot in &mut quotient {
        let value = remainder * 256 + 0xFF;
        *slot = (value / divisor).min(0xFF) as u8;
        remainder = value % divisor;
    }
    U256::from_big_endian(&quotient)
}

pub fn pow_hash_meets_target(pow_hash: &[u8], target: U256) -> bool {
    U256::from_big_endian(pow_hash) <= target
}

pub fn pow_target_to_hex(target: U256) -> String {
    format!("0x{}", hex::encode(target.to_big_endian()))
}

pub fn pow_target_from_hex(input: &str) -> Result<U256, String> {
    let decoded = hex::decode(input.strip_prefix("0x").unwrap_or(input))
        .map_err(|err| format!("invalid pow target hex: {err}"))?;
    if decoded.len() != 32 {
        return Err(format!(
            "pow target must decode to 32 bytes, got {}",
            decoded.len()
        ));
    }
    Ok(U256::from_big_endian(&decoded))
}

fn compute_pow_hash(header: &BlockHeader, nonce: u64) -> Hash {
    let mut data = header.encode_legacy_hash_preimage();
    data.extend_from_slice(&nonce.to_be_bytes());
    Hash::from_bytes(crate::crypto::keccak256(&data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block() {
        let genesis = create_genesis_block();

        assert_eq!(genesis.header.number, U256::zero());
        assert_eq!(genesis.header.parent_hash, Hash::zero());
        assert_eq!(genesis.body.as_ref().map(BlockBody::is_empty), Some(true));
    }

    #[test]
    fn test_pow_target_uses_full_256_bit_comparison() {
        let mut target_bytes = [0xFFu8; 32];
        target_bytes[..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x10]);
        let target = U256::from_big_endian(&target_bytes);
        let mut below = [0u8; 32];
        below[..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x10]);
        let mut above = [0u8; 32];
        above[..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x11]);

        assert!(pow_hash_meets_target(&below, target));
        assert!(!pow_hash_meets_target(&above, target));
    }

    #[test]
    fn test_pow_target_from_difficulty_is_continuous_below_byte_boundary() {
        let target = pow_target_from_difficulty(U256::from_u128(1_000_000));
        assert!(!target.is_zero());
        assert_eq!(target.leading_zeros() / 8, 2);
        assert!(
            target
                < U256::from_big_endian(&[
                    0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                    0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                ])
        );
    }
}
