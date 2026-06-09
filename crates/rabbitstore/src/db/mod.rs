//! Database abstraction layer

use crate::{Result, StorageError};
use redb::ReadableTable;
use std::sync::Arc;

/// Prefix iterator type used by key-value backends.
pub type PrefixIterator<'a> = Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)> + 'a>;
const REDB_KV_TABLE: redb::TableDefinition<&[u8], &[u8]> =
    redb::TableDefinition::new("rabbitstore_kv");

/// Key-value database trait
pub trait KeyValueDB: Send + Sync {
    /// Get value by key
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    /// Put key-value pair
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
    /// Delete key
    fn delete(&self, key: &[u8]) -> Result<()>;
    /// Check if key exists
    fn has(&self, key: &[u8]) -> Result<bool>;
    /// Write batch
    fn write_batch(&self, batch: Batch) -> Result<()>;
    /// Create batch
    fn batch(&self) -> Batch;
    /// Iterate over prefix
    fn iter_prefix(&self, prefix: &[u8]) -> Result<PrefixIterator<'_>>;
    /// Visit every item under a prefix without requiring callers to materialize all entries.
    fn for_each_prefix(
        &self,
        prefix: &[u8],
        visitor: &mut dyn FnMut(&[u8], &[u8]) -> Result<()>,
    ) -> Result<()> {
        for (key, value) in self.iter_prefix(prefix)? {
            visitor(&key, &value)?;
        }
        Ok(())
    }
}

/// Batch write operation
pub struct Batch {
    operations: Vec<BatchOp>,
}

enum BatchOp {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

impl Batch {
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
        }
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.operations
            .push(BatchOp::Put(key.to_vec(), value.to_vec()));
    }

    pub fn delete(&mut self, key: &[u8]) {
        self.operations.push(BatchOp::Delete(key.to_vec()));
    }
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

/// RocksDB implementation
pub struct RocksDb {
    db: rocksdb::DB,
}

/// Compression profile used when opening RocksDB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RocksDbCompression {
    /// LZ4, retained for legacy footprint comparisons.
    Lz4,
    /// ZSTD, the production default.
    Zstd,
}

impl RocksDbCompression {
    fn as_rocksdb(self) -> rocksdb::DBCompressionType {
        match self {
            Self::Lz4 => rocksdb::DBCompressionType::Lz4,
            Self::Zstd => rocksdb::DBCompressionType::Zstd,
        }
    }
}

impl RocksDb {
    pub fn open(path: &str) -> Result<Self> {
        Self::open_with_compression(path, RocksDbCompression::Zstd)
    }

    pub fn open_with_compression(path: &str, compression: RocksDbCompression) -> Result<Self> {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        // Use std::thread::available_parallelism instead of num_cpus
        let parallelism = std::thread::available_parallelism()
            .map(|p| p.get() as i32)
            .unwrap_or(4);
        opts.increase_parallelism(parallelism);
        opts.set_max_background_jobs(8);
        opts.set_write_buffer_size(256 * 1024 * 1024); // 256MB
        opts.set_max_write_buffer_number(4);
        opts.set_target_file_size_base(128 * 1024 * 1024); // 128MB

        // Favor disk footprint for production node state. Hot values are already compact binary.
        opts.set_compression_type(compression.as_rocksdb());

        // Bloom filter
        let mut block_opts = rocksdb::BlockBasedOptions::default();
        block_opts.set_bloom_filter(10.0, false);
        block_opts.set_block_size(16 * 1024); // 16KB
        opts.set_block_based_table_factory(&block_opts);

        let db =
            rocksdb::DB::open(&opts, path).map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(Self { db })
    }

    pub fn inner(&self) -> &rocksdb::DB {
        &self.db
    }

    pub fn flush(&self) -> Result<()> {
        self.db
            .flush()
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    pub fn compact_all(&self) {
        self.db.compact_range::<&[u8], &[u8]>(None, None);
    }
}

impl KeyValueDB for RocksDb {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.db
            .get(key)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.db
            .put(key, value)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        self.db
            .delete(key)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn has(&self, key: &[u8]) -> Result<bool> {
        Ok(self
            .db
            .get(key)
            .map_err(|e| StorageError::Database(e.to_string()))?
            .is_some())
    }

    fn write_batch(&self, batch: Batch) -> Result<()> {
        let mut db_batch = rocksdb::WriteBatch::default();

        for op in batch.operations {
            match op {
                BatchOp::Put(k, v) => {
                    db_batch.put(&k, &v);
                }
                BatchOp::Delete(k) => {
                    db_batch.delete(&k);
                }
            }
        }

        self.db
            .write(db_batch)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn batch(&self) -> Batch {
        Batch::new()
    }

    fn iter_prefix(&self, prefix: &[u8]) -> Result<PrefixIterator<'_>> {
        let iter = self.db.prefix_iterator(prefix);
        let mut items = Vec::new();

        for item in iter {
            let (k, v) = item.map_err(|e| StorageError::Database(e.to_string()))?;
            if !k.starts_with(prefix) {
                break;
            }
            items.push((k.to_vec(), v.to_vec()));
        }

        Ok(Box::new(items.into_iter()))
    }

    fn for_each_prefix(
        &self,
        prefix: &[u8],
        visitor: &mut dyn FnMut(&[u8], &[u8]) -> Result<()>,
    ) -> Result<()> {
        for item in self.db.prefix_iterator(prefix) {
            let (key, value) = item.map_err(|e| StorageError::Database(e.to_string()))?;
            if !key.starts_with(prefix) {
                break;
            }
            visitor(&key, &value)?;
        }
        Ok(())
    }
}

/// Redb implementation (pure Rust alternative)
pub struct RedbDatabase {
    db: Arc<redb::Database>,
}

impl RedbDatabase {
    pub fn open(path: &str) -> Result<Self> {
        let db = redb::Database::create(path).map_err(|e| StorageError::Database(e.to_string()))?;
        let tx = db
            .begin_write()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        {
            tx.open_table(REDB_KV_TABLE)
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(Self { db: Arc::new(db) })
    }
}

impl KeyValueDB for RedbDatabase {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let table = tx
            .open_table(REDB_KV_TABLE)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let value = table
            .get(key)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(value.map(|guard| guard.value().to_vec()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let tx = self
            .db
            .begin_write()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        {
            let mut table = tx
                .open_table(REDB_KV_TABLE)
                .map_err(|e| StorageError::Database(e.to_string()))?;
            table
                .insert(key, value)
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        let tx = self
            .db
            .begin_write()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        {
            let mut table = tx
                .open_table(REDB_KV_TABLE)
                .map_err(|e| StorageError::Database(e.to_string()))?;
            table
                .remove(key)
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(())
    }

    fn has(&self, key: &[u8]) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    fn write_batch(&self, batch: Batch) -> Result<()> {
        let tx = self
            .db
            .begin_write()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        {
            let mut table = tx
                .open_table(REDB_KV_TABLE)
                .map_err(|e| StorageError::Database(e.to_string()))?;
            for op in batch.operations {
                match op {
                    BatchOp::Put(k, v) => {
                        table
                            .insert(k.as_slice(), v.as_slice())
                            .map_err(|e| StorageError::Database(e.to_string()))?;
                    }
                    BatchOp::Delete(k) => {
                        table
                            .remove(k.as_slice())
                            .map_err(|e| StorageError::Database(e.to_string()))?;
                    }
                }
            }
        }
        tx.commit()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(())
    }

    fn batch(&self) -> Batch {
        Batch::new()
    }

    fn iter_prefix(&self, prefix: &[u8]) -> Result<PrefixIterator<'_>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let table = tx
            .open_table(REDB_KV_TABLE)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let mut iter = table
            .iter()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let mut items = Vec::new();
        for entry in iter {
            let (k, v) = entry.map_err(|e| StorageError::Database(e.to_string()))?;
            if k.value().starts_with(prefix) {
                items.push((k.value().to_vec(), v.value().to_vec()));
            }
        }
        Ok(Box::new(items.into_iter()))
    }

    fn for_each_prefix(
        &self,
        prefix: &[u8],
        visitor: &mut dyn FnMut(&[u8], &[u8]) -> Result<()>,
    ) -> Result<()> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let table = tx
            .open_table(REDB_KV_TABLE)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let mut iter = table
            .iter()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        for entry in iter {
            let (key, value) = entry.map_err(|e| StorageError::Database(e.to_string()))?;
            if key.value().starts_with(prefix) {
                visitor(key.value(), value.value())?;
            }
        }
        Ok(())
    }
}

/// In-memory database (for testing)
pub struct MemDatabase {
    data: parking_lot::RwLock<std::collections::HashMap<Vec<u8>, Vec<u8>>>,
}

impl MemDatabase {
    pub fn new() -> Self {
        Self {
            data: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for MemDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyValueDB for MemDatabase {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.data.read().get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.data.write().insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        self.data.write().remove(key);
        Ok(())
    }

    fn has(&self, key: &[u8]) -> Result<bool> {
        Ok(self.data.read().contains_key(key))
    }

    fn write_batch(&self, batch: Batch) -> Result<()> {
        for op in batch.operations {
            match op {
                BatchOp::Put(k, v) => {
                    self.data.write().insert(k, v);
                }
                BatchOp::Delete(k) => {
                    self.data.write().remove(&k);
                }
            }
        }
        Ok(())
    }

    fn batch(&self) -> Batch {
        Batch::new()
    }

    fn iter_prefix(&self, prefix: &[u8]) -> Result<PrefixIterator<'_>> {
        let prefix = prefix.to_vec();
        let items: Vec<_> = self
            .data
            .read()
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Ok(Box::new(items.into_iter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_mem_database() {
        let db = MemDatabase::new();

        db.put(b"key1", b"value1").unwrap();
        assert_eq!(db.get(b"key1").unwrap(), Some(b"value1".to_vec()));
        assert!(db.has(b"key1").unwrap());

        db.delete(b"key1").unwrap();
        assert_eq!(db.get(b"key1").unwrap(), None);
    }

    #[test]
    fn test_batch_write() {
        let db = MemDatabase::new();
        let mut batch = db.batch();

        batch.put(b"key1", b"value1");
        batch.put(b"key2", b"value2");
        batch.delete(b"key3");

        db.write_batch(batch).unwrap();

        assert_eq!(db.get(b"key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(db.get(b"key2").unwrap(), Some(b"value2".to_vec()));
    }

    #[test]
    fn test_redb_database_roundtrip_and_prefix() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("rabbitstore-redb.db");
        let path_s = path.to_string_lossy().to_string();

        let db = RedbDatabase::open(&path_s).expect("open redb");
        db.put(b"k1", b"v1").expect("put k1");
        db.put(b"prefix:a", b"va").expect("put prefix:a");
        db.put(b"prefix:b", b"vb").expect("put prefix:b");
        assert_eq!(db.get(b"k1").expect("get k1"), Some(b"v1".to_vec()));
        assert!(db.has(b"k1").expect("has k1"));

        let mut batch = db.batch();
        batch.put(b"k2", b"v2");
        batch.delete(b"k1");
        db.write_batch(batch).expect("write batch");

        assert_eq!(db.get(b"k2").expect("get k2"), Some(b"v2".to_vec()));
        assert_eq!(db.get(b"k1").expect("get deleted k1"), None);

        let mut prefixed: Vec<_> = db.iter_prefix(b"prefix:").expect("iter prefix").collect();
        prefixed.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(prefixed.len(), 2);
        assert_eq!(prefixed[0], (b"prefix:a".to_vec(), b"va".to_vec()));
        assert_eq!(prefixed[1], (b"prefix:b".to_vec(), b"vb".to_vec()));
    }
}
