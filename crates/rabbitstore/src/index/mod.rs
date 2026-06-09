//! Index services for fast lookups

use crate::db::{Batch, KeyValueDB};
use crate::{Result, StorageError};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use rabbitcore::crypto::{Address, Hash};

/// Operation index entry
#[derive(Clone, Debug)]
pub struct TxIndex {
    /// Block hash
    pub block_hash: Hash,
    /// Block number
    pub block_number: u64,
    /// Operation index in block
    pub index: u32,
}

impl TxIndex {
    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(self.block_hash.as_bytes());
        data.extend_from_slice(&self.block_number.to_be_bytes());
        data.extend_from_slice(&self.index.to_be_bytes());
        data
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 44 {
            return Err(StorageError::Serialization("Invalid tx index data".into()));
        }

        let block_hash = Hash::from_slice(&data[0..32])?;
        let block_number = u64::from_be_bytes(
            data[32..40]
                .try_into()
                .map_err(|_| StorageError::Serialization("Invalid tx index data".into()))?,
        );
        let index = u32::from_be_bytes(
            data[40..44]
                .try_into()
                .map_err(|_| StorageError::Serialization("Invalid tx index data".into()))?,
        );

        Ok(Self {
            block_hash,
            block_number,
            index,
        })
    }
}

/// Block index entry
#[derive(Clone, Debug)]
pub struct BlockIndex {
    /// Block number
    pub number: u64,
    /// Block hash
    pub hash: Hash,
    /// Total difficulty
    pub total_difficulty: u128,
}

/// Index database
pub struct IndexDB {
    db: Arc<dyn KeyValueDB>,
    /// In-memory cache for latest indices
    tx_cache: RwLock<HashMap<Hash, TxIndex>>,
    block_cache: RwLock<HashMap<u64, Hash>>,
    /// Latest block number
    latest_block: RwLock<u64>,
}

impl IndexDB {
    pub fn new(db: Arc<dyn KeyValueDB>) -> Self {
        Self {
            db,
            tx_cache: RwLock::new(HashMap::new()),
            block_cache: RwLock::new(HashMap::new()),
            latest_block: RwLock::new(0),
        }
    }

    /// Index operation
    pub fn index_operation(
        &self,
        tx_hash: Hash,
        block_hash: Hash,
        block_number: u64,
        index: u32,
    ) -> Result<()> {
        let key = self.tx_key(&tx_hash);
        let tx_index = TxIndex {
            block_hash,
            block_number,
            index,
        };

        self.db.put(&key, &tx_index.encode())?;
        self.tx_cache.write().insert(tx_hash, tx_index);

        Ok(())
    }

    /// Index block
    pub fn index_block(&self, number: u64, hash: Hash, total_difficulty: u128) -> Result<()> {
        // Store hash by number
        let hash_key = self.block_hash_key(number);
        self.db.put(&hash_key, hash.as_bytes())?;

        // Store number by hash
        let number_key = self.block_number_key(&hash);
        self.db.put(&number_key, &number.to_be_bytes())?;

        // Update caches
        self.block_cache.write().insert(number, hash);
        *self.latest_block.write() = number;

        Ok(())
    }

    /// Get operation by hash
    pub fn get_operation(&self, tx_hash: &Hash) -> Result<Option<TxIndex>> {
        // Check cache first
        if let Some(index) = self.tx_cache.read().get(tx_hash) {
            return Ok(Some(index.clone()));
        }

        // Load from database
        let key = self.tx_key(tx_hash);
        match self.db.get(&key)? {
            Some(data) => {
                let index = TxIndex::decode(&data)?;
                self.tx_cache.write().insert(*tx_hash, index.clone());
                Ok(Some(index))
            }
            None => Ok(None),
        }
    }

    /// Get block hash by number
    pub fn get_block_hash(&self, number: u64) -> Result<Option<Hash>> {
        // Check cache first
        if let Some(hash) = self.block_cache.read().get(&number) {
            return Ok(Some(*hash));
        }

        // Load from database
        let key = self.block_hash_key(number);
        match self.db.get(&key)? {
            Some(data) => {
                let hash = Hash::from_slice(&data)?;
                self.block_cache.write().insert(number, hash);
                Ok(Some(hash))
            }
            None => Ok(None),
        }
    }

    /// Get block number by hash
    pub fn get_block_number(&self, hash: &Hash) -> Result<Option<u64>> {
        let key = self.block_number_key(hash);
        match self.db.get(&key)? {
            Some(data) => {
                let number = u64::from_be_bytes(data.as_slice().try_into().map_err(|_| {
                    StorageError::Serialization("Invalid block number index data".into())
                })?);
                Ok(Some(number))
            }
            None => Ok(None),
        }
    }

    /// Get latest block number
    pub fn latest_block(&self) -> u64 {
        *self.latest_block.read()
    }

    /// Batch index operations
    pub fn batch_index_operations(&self, txs: &[(Hash, Hash, u64, u32)]) -> Result<()> {
        let mut batch = self.db.batch();

        for (tx_hash, block_hash, block_number, index) in txs {
            let key = self.tx_key(tx_hash);
            let tx_index = TxIndex {
                block_hash: *block_hash,
                block_number: *block_number,
                index: *index,
            };

            batch.put(&key, &tx_index.encode());
            self.tx_cache.write().insert(*tx_hash, tx_index);
        }

        self.db.write_batch(batch)
    }

    fn tx_key(&self, tx_hash: &Hash) -> Vec<u8> {
        let mut key = Vec::with_capacity(33);
        key.push(b't');
        key.extend_from_slice(tx_hash.as_bytes());
        key
    }

    fn block_hash_key(&self, number: u64) -> Vec<u8> {
        let mut key = Vec::with_capacity(9);
        key.push(b'h');
        key.extend_from_slice(&number.to_be_bytes());
        key
    }

    fn block_number_key(&self, hash: &Hash) -> Vec<u8> {
        let mut key = Vec::with_capacity(33);
        key.push(b'n');
        key.extend_from_slice(hash.as_bytes());
        key
    }
}

/// Address operation index
pub struct AddressTxIndex {
    db: Arc<dyn KeyValueDB>,
}

impl AddressTxIndex {
    pub fn new(db: Arc<dyn KeyValueDB>) -> Self {
        Self { db }
    }

    /// Index operation for address
    pub fn index_op_for_address(
        &self,
        address: &Address,
        tx_hash: Hash,
        block_number: u64,
    ) -> Result<()> {
        let key = self.address_tx_key(address, block_number, &tx_hash);
        self.db.put(&key, tx_hash.as_bytes())
    }

    /// Get operations for address
    pub fn get_operations_for_address(
        &self,
        address: &Address,
        from_block: Option<u64>,
        to_block: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Hash>> {
        let prefix = self.address_prefix(address);
        let mut txs = Vec::new();

        for (key, value) in self.db.iter_prefix(&prefix)? {
            // Parse block number from key
            // Check range
            // Add to results
            let tx_hash = Hash::from_slice(&value)?;
            txs.push(tx_hash);

            if txs.len() >= limit {
                break;
            }
        }

        Ok(txs)
    }

    fn address_tx_key(&self, address: &Address, block_number: u64, tx_hash: &Hash) -> Vec<u8> {
        let mut key = Vec::with_capacity(20 + 8 + 32 + 1);
        key.push(b'a');
        key.extend_from_slice(address.as_bytes());
        key.extend_from_slice(&block_number.to_be_bytes());
        key.extend_from_slice(tx_hash.as_bytes());
        key
    }

    fn address_prefix(&self, address: &Address) -> Vec<u8> {
        let mut prefix = Vec::with_capacity(21);
        prefix.push(b'a');
        prefix.extend_from_slice(address.as_bytes());
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MemDatabase;

    #[test]
    fn test_tx_index() {
        let db = Arc::new(MemDatabase::new());
        let index_db = IndexDB::new(db);

        let tx_hash = Hash::from_bytes([1u8; 32]);
        let block_hash = Hash::from_bytes([2u8; 32]);

        index_db
            .index_operation(tx_hash, block_hash, 100, 0)
            .unwrap();

        let retrieved = index_db.get_operation(&tx_hash).unwrap().unwrap();
        assert_eq!(retrieved.block_hash, block_hash);
        assert_eq!(retrieved.block_number, 100);
        assert_eq!(retrieved.index, 0);
    }

    #[test]
    fn test_block_index() {
        let db = Arc::new(MemDatabase::new());
        let index_db = IndexDB::new(db);

        let hash = Hash::from_bytes([1u8; 32]);
        index_db.index_block(100, hash, 1000000).unwrap();

        let retrieved_hash = index_db.get_block_hash(100).unwrap().unwrap();
        assert_eq!(retrieved_hash, hash);

        let retrieved_number = index_db.get_block_number(&hash).unwrap().unwrap();
        assert_eq!(retrieved_number, 100);

        assert_eq!(index_db.latest_block(), 100);
    }
}
