//! Block module

use crate::account::U256;
use crate::compute::{ComputeTx, OutputId, TxId};
use crate::crypto::{Address, Hash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CANONICAL_BLOCK_VERSION: u32 = 3;
pub const DIFFICULTY_WINDOW_BLOCKS: usize = 10;
pub const TARGET_BLOCK_INTERVAL_SECS: u64 = 10;
pub const BASE_MINING_DIFFICULTY: u128 = 1;
pub const MIN_MINING_DIFFICULTY: u128 = 1;
pub const MAX_MINING_DIFFICULTY: u128 = 1_000_000_000;
pub const DIFFICULTY_BOOTSTRAP_THRESHOLD: u128 = 1_000_000;
pub const DIFFICULTY_BOOTSTRAP_STEP_BPS: u128 = 40_000;
pub const DIFFICULTY_MAX_STEP_BPS: u128 = 2_500;
const DIFFICULTY_INTERVAL_EMA_WEIGHT: u128 = 4;

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
        let encoded = self.encode_canonical_hash_preimage();
        Hash::from_bytes(crate::crypto::keccak256(&encoded))
    }

    pub fn validate(&self, parent: &BlockHeader) -> Result<(), BlockError> {
        let parent_hash = if parent.hash.is_zero() {
            parent.canonical_hash()
        } else {
            parent.hash
        };
        if self.parent_hash != parent_hash {
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
        // Genesis is exempt from PoW; every non-genesis block must meet a
        // non-zero difficulty floor (fail-fast: never treat difficulty=0 as valid work).
        if self.number.is_zero() {
            return Ok(());
        }
        if self.difficulty.is_zero() {
            return Err(BlockError::InvalidDifficulty);
        }
        let target = pow_target_from_difficulty(self.difficulty);
        let pow_hash = compute_pow_hash(self, self.nonce);

        if !pow_hash_meets_target(pow_hash.as_bytes(), target) {
            return Err(BlockError::InvalidPow);
        }

        Ok(())
    }

    /// Recompute and cache the canonical header hash.
    pub fn seal_hash(&mut self) {
        self.hash = self.compute_hash();
    }

    /// Canonical header hash (never trusts the skipped serde cache blindly).
    pub fn canonical_hash(&self) -> Hash {
        self.compute_hash()
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

    /// Returns the canonical hash preimage bytes used for both header hashing
    /// and PoW computation.
    pub fn compute_hash_preimage_for_pow(&self) -> Vec<u8> {
        self.encode_canonical_hash_preimage()
    }

    fn encode_canonical_hash_preimage(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.version.to_be_bytes());
        data.extend_from_slice(self.parent_hash.as_bytes());
        data.extend_from_slice(&(self.uncle_hashes.len() as u64).to_be_bytes());
        for uncle in &self.uncle_hashes {
            data.extend_from_slice(uncle.as_bytes());
        }
        data.extend_from_slice(self.coinbase.as_bytes());
        data.extend_from_slice(self.state_root.as_bytes());
        data.extend_from_slice(self.transactions_root.as_bytes());
        data.extend_from_slice(self.receipts_root.as_bytes());
        data.extend_from_slice(&self.number.to_big_endian());
        data.extend_from_slice(&self.gas_limit.to_be_bytes());
        data.extend_from_slice(&self.gas_used.to_be_bytes());
        data.extend_from_slice(&self.timestamp.to_be_bytes());
        data.extend_from_slice(&self.difficulty.to_big_endian());
        data.extend_from_slice(&self.nonce.to_be_bytes());
        data.extend_from_slice(&(self.extra_data.len() as u64).to_be_bytes());
        data.extend_from_slice(&self.extra_data);
        data.extend_from_slice(self.mix_hash.as_bytes());
        data.extend_from_slice(&self.base_fee_per_gas.to_big_endian());
        data
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

/// Canonical execution receipt stored alongside a block body.
///
/// Note: `block_hash` is excluded from the receipt commitment root computation
/// (`compute_receipts_root`) to avoid a circular dependency (header hash depends
/// on the receipts root, which would otherwise depend on the block hash).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_id: TxId,
    #[serde(skip)]
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
///
/// The receipt's `block_hash` field is intentionally **excluded** from the
/// commitment encoding. This breaks the circular dependency between
/// `header.hash` (which binds `receipts_root`) and `receipt.block_hash`
/// (which is the header hash). `block_hash` is a post-hoc annotation for
/// external consumers and must never influence consensus state.
pub fn compute_receipts_root(receipts: &[Receipt]) -> Hash {
    let leaves = receipts
        .iter()
        .map(|receipt| {
            let mut commitment = receipt.clone();
            commitment.block_hash = Hash::zero(); // strip circular field
            commitment_leaf_hash(b"RABBIT-BLOCK-RECEIPT-V1", &commitment)
        })
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
        version: CANONICAL_BLOCK_VERSION,
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
        difficulty: U256::zero(),
        nonce: 0,
        extra_data: b"RabbitChain Genesis".to_vec(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(crate::compute::INITIAL_BASE_FEE),
        hash: Hash::zero(),
    };

    let hash = header.compute_hash();
    let mut header = header;
    header.hash = hash;
    Block::new(header).with_body(BlockBody::default())
}

/// Adjust mining difficulty from a smoothed recent block window.
pub fn calculate_windowed_mining_difficulty(headers: &[BlockHeader]) -> U256 {
    let Some(latest) = headers.last() else {
        return U256::from_u128(BASE_MINING_DIFFICULTY);
    };

    let observed_block_time = smoothed_recent_block_interval(headers)
        .unwrap_or(TARGET_BLOCK_INTERVAL_SECS.max(1) as u128);

    adjust_mining_difficulty(
        latest.difficulty,
        observed_block_time,
        TARGET_BLOCK_INTERVAL_SECS,
        MIN_MINING_DIFFICULTY,
        MAX_MINING_DIFFICULTY,
        DIFFICULTY_BOOTSTRAP_THRESHOLD,
    )
}

/// Core difficulty controller shared by block production, sync validation and consensus tests.
pub(crate) fn adjust_mining_difficulty(
    parent_difficulty: U256,
    observed_block_time_secs: u128,
    target_block_time_secs: u64,
    min_mining_difficulty: u128,
    max_mining_difficulty: u128,
    bootstrap_threshold: u128,
) -> U256 {
    let parent = parent_difficulty
        .as_u128()
        .max(min_mining_difficulty)
        .clamp(min_mining_difficulty, max_mining_difficulty);
    let observed = observed_block_time_secs.max(1);
    let target = target_block_time_secs.max(1) as u128;
    let candidate = parent
        .saturating_mul(target)
        .saturating_div(observed)
        .clamp(min_mining_difficulty, max_mining_difficulty);

    let step_bps = if parent < bootstrap_threshold {
        DIFFICULTY_BOOTSTRAP_STEP_BPS
    } else {
        DIFFICULTY_MAX_STEP_BPS
    };
    let step = parent.saturating_mul(step_bps).saturating_div(10_000).max(1);
    let min_next = parent.saturating_sub(step).max(min_mining_difficulty);
    let max_next = parent.saturating_add(step).min(max_mining_difficulty);

    U256::from_u128(candidate.clamp(min_next, max_next))
}

fn smoothed_recent_block_interval(headers: &[BlockHeader]) -> Option<u128> {
    let window_len = headers.len().min(DIFFICULTY_WINDOW_BLOCKS);
    if window_len < 2 {
        return None;
    }

    let window = &headers[headers.len() - window_len..];
    let mut smoothed_interval: Option<u128> = None;

    for pair in window.windows(2) {
        let interval = pair[1]
            .timestamp
            .saturating_sub(pair[0].timestamp)
            .max(1) as u128;
        smoothed_interval = Some(match smoothed_interval {
            None => interval,
            Some(previous) => {
                ((previous * (DIFFICULTY_INTERVAL_EMA_WEIGHT - 1)) + interval)
                    / DIFFICULTY_INTERVAL_EMA_WEIGHT
            }
        });
    }

    smoothed_interval.map(|interval| interval.max(1))
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

/// Domain-separated tag for PoW hashing (distinct from block hash preimage).
const POW_DOMAIN_TAG: &[u8] = b"RABBIT-POW-V1";

/// Compute the PoW hash for a block header and nonce.
///
/// RabbitChain PoW is **SHA-256d** (double SHA-256), the same hash function
/// family used by Bitcoin, which allows SHA-256 ASIC hardware (via a
/// compatible mining bridge) to participate. The preimage binds the full
/// header commitment fields (except the cached hash) so coinbase / roots /
/// difficulty / extra_data cannot be swapped after mining:
///
/// ```text
/// sha256( sha256( "RABBIT-POW-V1" || encode_canonical_hash_preimage(header_with_nonce) ) )
/// ```
///
/// `header.nonce` is temporarily treated as `nonce` for encoding so miners can
/// trial nonces without mutating the stored header first.
pub fn compute_pow_hash(header: &BlockHeader, nonce: u64) -> Hash {
    use sha2::{Digest, Sha256};

    let mut sealed = header.clone();
    sealed.nonce = nonce;
    // mix_hash is the PoW output field: always zero it in the preimage so the
    // digest does not depend on itself (standard mix_hash / seal pattern).
    sealed.mix_hash = Hash::zero();
    // hash field is not part of encode_canonical_hash_preimage
    let mut data = Vec::with_capacity(16 + 256);
    data.extend_from_slice(POW_DOMAIN_TAG);
    data.extend_from_slice(&sealed.encode_canonical_hash_preimage());

    // SHA-256d: first pass, then second pass over the first digest.
    let first = Sha256::digest(&data);
    let second = Sha256::digest(&first);
    Hash::from_bytes(second.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block() {
        let genesis = create_genesis_block();

        assert_eq!(genesis.header.number, U256::zero());
        assert_eq!(genesis.header.parent_hash, Hash::zero());
        assert_eq!(genesis.header.version, CANONICAL_BLOCK_VERSION);
        assert_eq!(genesis.body.as_ref().map(BlockBody::is_empty), Some(true));
    }

    #[test]
    fn test_windowed_mining_difficulty_uses_recent_block_times() {
        let mut headers = Vec::new();
        for i in 0..10u64 {
            headers.push(BlockHeader {
                version: CANONICAL_BLOCK_VERSION,
                parent_hash: Hash::zero(),
                uncle_hashes: Vec::new(),
                coinbase: Address::zero(),
                state_root: Hash::zero(),
                transactions_root: Hash::zero(),
                receipts_root: Hash::zero(),
                number: U256::from(i),
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: i * 10,
                difficulty: U256::from_u128(1_000_000),
                nonce: 0,
                extra_data: Vec::new(),
                mix_hash: Hash::zero(),
                base_fee_per_gas: U256::from(1_000_000_000u64),
                hash: Hash::zero(),
            });
        }

        let difficulty = calculate_windowed_mining_difficulty(&headers);
        assert_eq!(difficulty, U256::from_u128(1_000_000));

        let mut fast_headers = headers.clone();
        for (idx, header) in fast_headers.iter_mut().enumerate() {
            header.timestamp = (idx as u64) * 5;
        }
        let faster = calculate_windowed_mining_difficulty(&fast_headers);
        assert_eq!(faster, U256::from_u128(1_250_000));

        let mut slow_headers = headers.clone();
        for (idx, header) in slow_headers.iter_mut().enumerate() {
            header.timestamp = (idx as u64) * 20;
        }
        let slower = calculate_windowed_mining_difficulty(&slow_headers);
        assert_eq!(slower, U256::from_u128(750_000));
    }

    #[test]
    fn test_windowed_mining_difficulty_bootstraps_from_low_difficulty() {
        let mut headers = Vec::new();
        for i in 0..10u64 {
            headers.push(BlockHeader {
                version: CANONICAL_BLOCK_VERSION,
                parent_hash: Hash::zero(),
                uncle_hashes: Vec::new(),
                coinbase: Address::zero(),
                state_root: Hash::zero(),
                transactions_root: Hash::zero(),
                receipts_root: Hash::zero(),
                number: U256::from(i),
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: i,
                difficulty: U256::from_u128(1),
                nonce: 0,
                extra_data: Vec::new(),
                mix_hash: Hash::zero(),
                base_fee_per_gas: U256::from(1_000_000_000u64),
                hash: Hash::zero(),
            });
        }

        let difficulty = calculate_windowed_mining_difficulty(&headers);
        assert_eq!(difficulty, U256::from_u128(5));
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

    #[test]
    fn test_compute_pow_hash_binds_full_header_and_nonce() {
        let header = BlockHeader {
            version: 3,
            parent_hash: Hash::from_bytes([1u8; 32]),
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(42u64),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1000,
            difficulty: U256::from_u128(1000),
            nonce: 7,
            extra_data: Vec::new(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        let computed = compute_pow_hash(&header, 7u64);
        let mut sealed = header.clone();
        sealed.nonce = 7;
        let mut expected = Vec::new();
        expected.extend_from_slice(POW_DOMAIN_TAG);
        expected.extend_from_slice(&sealed.encode_canonical_hash_preimage());
        use sha2::{Digest, Sha256};
        let first = Sha256::digest(&expected);
        let second = Sha256::digest(&first);
        assert_eq!(computed, Hash::from_bytes(second.into()));
        let diff = compute_pow_hash(&header, 8u64);
        assert_ne!(computed, diff);

        // Changing coinbase must change the PoW digest (content binding).
        let mut other = header.clone();
        other.coinbase = Address::from_bytes([0xab; 20]);
        assert_ne!(computed, compute_pow_hash(&other, 7u64));
    }

    #[test]
    fn test_verify_pow_rejects_impossible_difficulty() {
        let mut header = BlockHeader {
            version: 3,
            parent_hash: Hash::from_bytes([2u8; 32]),
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1100,
            difficulty: U256::from_u128(u128::MAX),
            nonce: 0,
            extra_data: Vec::new(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        header.hash = header.compute_hash();
        assert!(header.verify_pow().is_err());
    }

    #[test]
    fn test_verify_pow_rejects_zero_difficulty_for_non_genesis() {
        let mut header = BlockHeader {
            version: 3,
            parent_hash: Hash::from_bytes([3u8; 32]),
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1200,
            difficulty: U256::zero(),
            nonce: 999,
            extra_data: Vec::new(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        header.seal_hash();
        assert_eq!(header.verify_pow(), Err(BlockError::InvalidDifficulty));
    }

    fn test_verify_pow_allows_genesis_zero_difficulty() {
        let genesis = create_genesis_block();
        assert!(genesis.header.difficulty.is_zero());
        assert!(genesis.header.verify_pow().is_ok());
    }

    #[test]
    fn test_golden_pow_v1_vectors() {
        // Keep in sync with fixtures/pow/golden_pow_v1.json and mining-stack.
        let vectors = [
            (
                "empty-template-nonce-42",
                3u32,
                [0x11u8; 32],
                [0x22u8; 20],
                [0u8; 32],
                1u64,
                30_000_000u64,
                1_700_000_000u64,
                1u128,
                42u64,
                1_000_000_000u128,
                "a388c62f5c894dd02d6b63d0197a3268ce3fcbd541561c85408612ebdc516683",
            ),
            (
                "empty-template-coinbase-changed",
                3,
                [0x11u8; 32],
                [0x33u8; 20],
                [0u8; 32],
                1,
                30_000_000,
                1_700_000_000,
                1,
                42,
                1_000_000_000,
                "6894721822c61f50aae85d3e354ed46f0d98829562d42fcc6f400ff54442ec07",
            ),
            (
                "empty-template-nonce-43",
                3,
                [0x11u8; 32],
                [0x22u8; 20],
                [0u8; 32],
                1,
                30_000_000,
                1_700_000_000,
                1,
                43,
                1_000_000_000,
                "197ccde8355d18f08872712652764dc01f9c291af001f10ad22bbb59a43da2ee",
            ),
            (
                "nonzero-state-root-height-7",
                3,
                [0x11u8; 32],
                [0x22u8; 20],
                [0xaau8; 32],
                7,
                30_000_000,
                99,
                0x100,
                0,
                1_000_000_000,
                "66db97e06a88758bd427d173ab74e7e6390d12a705916c3dec72a2d0b117c7af",
            ),
        ];

        for (id, version, parent, coinbase, state, number, gas_limit, ts, diff, nonce, base_fee, expected_hex) in vectors {
            let header = BlockHeader {
                version,
                parent_hash: Hash::from_bytes(parent),
                uncle_hashes: Vec::new(),
                coinbase: Address::from_bytes(coinbase),
                state_root: Hash::from_bytes(state),
                transactions_root: Hash::zero(),
                receipts_root: Hash::zero(),
                number: U256::from(number),
                gas_limit,
                gas_used: 0,
                timestamp: ts,
                difficulty: U256::from_u128(diff),
                nonce,
                extra_data: Vec::new(),
                mix_hash: Hash::zero(),
                base_fee_per_gas: U256::from_u128(base_fee),
                hash: Hash::zero(),
            };
            let got = compute_pow_hash(&header, nonce);
            let expected = Hash::from_hex(expected_hex).expect(id);
            assert_eq!(got, expected, "golden vector mismatch: {id}");
        }

        // Content binding: coinbase change must diverge.
        let a = Hash::from_hex("a388c62f5c894dd02d6b63d0197a3268ce3fcbd541561c85408612ebdc516683").unwrap();
        let b = Hash::from_hex("6894721822c61f50aae85d3e354ed46f0d98829562d42fcc6f400ff54442ec07").unwrap();
        assert_ne!(a, b);
    }
}
