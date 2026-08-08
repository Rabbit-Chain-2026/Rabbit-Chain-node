//! Persistent storage for UTXO Compute objects and tx results.

use std::{collections::BTreeSet, sync::Arc};

use serde::{Deserialize, Serialize};
use rabbitcore::compute::{
    execution::ObjectStore,
    primitives::{ObjectId, OutputId, TxId},
    ObjectOutput,
};

use crate::{db::KeyValueDB, Result, StorageError};

const OUTPUT_PREFIX: &[u8] = b"compute:output:";
const LATEST_PREFIX: &[u8] = b"compute:latest:";
const TX_RESULT_PREFIX: &[u8] = b"compute:txresult:";
const REPLAY_PREFIX: &[u8] = b"compute:replay:";
const CHECKPOINT_PREFIX: &[u8] = b"compute:checkpoint:";
const OUTPUT_BINARY_MAGIC: &[u8; 4] = b"ZCO1";
const TX_RESULT_BINARY_MAGIC: &[u8; 4] = b"ZCR1";
const CHECKPOINT_BINARY_MAGIC: &[u8; 4] = b"ZCC1";

/// Binary envelope for persisted compute transaction results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComputeResultEnvelope {
    /// Canonical JSON result returned by the RPC layer.
    pub result_json: String,
}

/// Statistics returned by compute store rebuilds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComputeRebuildStats {
    /// Total key-value entries visited from the source database.
    pub total_entries: u64,
    /// Entries under the compute output namespace.
    pub output_entries: u64,
    /// Legacy JSON output entries decoded and rewritten to binary.
    pub legacy_json_outputs: u64,
    /// Already-binary output entries encountered.
    pub binary_outputs: u64,
    /// Non-output entries copied without codec changes.
    pub copied_entries: u64,
    /// Entries whose value changed in the rebuilt target database.
    pub rewritten_entries: u64,
    /// Entries under the compute tx-result namespace.
    pub tx_result_entries: u64,
    /// Legacy JSON tx-result entries decoded and rewritten to binary.
    pub legacy_json_tx_results: u64,
    /// Already-binary tx-result entries encountered.
    pub binary_tx_results: u64,
}

/// Retention policy for explicit compute store pruning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputePruneConfig {
    /// Keep all history. Used by archive and explorer nodes.
    pub retain_all: bool,
    /// Current unix timestamp used to evaluate retention windows.
    pub now_unix_secs: u64,
    /// Minimum age before spent outputs and tx results can be pruned.
    pub retention_window_secs: u64,
    /// Scan and report candidates without deleting entries.
    pub dry_run: bool,
}

/// Statistics returned by compute pruning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComputePruneStats {
    /// Total key-value entries visited.
    pub total_entries: u64,
    /// Compute output entries visited.
    pub output_entries: u64,
    /// Compute tx-result entries visited.
    pub tx_result_entries: u64,
    /// Old spent output entries eligible for pruning.
    pub spent_output_candidates: u64,
    /// Old compute tx-result entries eligible for pruning.
    pub tx_result_candidates: u64,
    /// Stale latest-output pointers removed with pruned spent outputs.
    pub latest_entries_deleted: u64,
    /// Entries actually deleted. This is zero in dry-run or retain-all mode.
    pub deleted_entries: u64,
}

/// Durable per-block journal entry for H6-B2 reorg rollback (persisted).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DurableComputeCheckpoint {
    pub height: u64,
    pub block_hash: [u8; 32],
    pub recorded_at_unix: u64,
    pub spent_output_ids: Vec<[u8; 32]>,
    pub created_outputs: Vec<ObjectOutput>,
    pub included_tx_ids: Vec<[u8; 32]>,
}

/// Durable backend for compute object outputs and tx results.
pub struct ComputeStore {
    db: Arc<dyn KeyValueDB>,
}

impl ComputeStore {
    /// Creates a new compute store over key-value DB.
    pub fn new(db: Arc<dyn KeyValueDB>) -> Self {
        Self { db }
    }

    /// Underlying KV (for diagnostics).
    pub fn db(&self) -> &Arc<dyn KeyValueDB> {
        &self.db
    }

    /// Persist a block-height checkpoint journal entry (H6-B2.1).
    pub fn put_checkpoint(&self, entry: &DurableComputeCheckpoint) -> Result<()> {
        let key = checkpoint_key(entry.height);
        let encoded = bincode::serialize(entry)
            .map_err(|e| StorageError::Serialization(format!("encode checkpoint: {e}")))?;
        let mut value = Vec::with_capacity(CHECKPOINT_BINARY_MAGIC.len() + encoded.len());
        value.extend_from_slice(CHECKPOINT_BINARY_MAGIC);
        value.extend_from_slice(&encoded);
        self.db.put(&key, &value)?;
        Ok(())
    }

    /// Load a checkpoint by height.
    pub fn get_checkpoint(&self, height: u64) -> Result<Option<DurableComputeCheckpoint>> {
        let key = checkpoint_key(height);
        let Some(bytes) = self.db.get(&key)? else {
            return Ok(None);
        };
        let payload = bytes
            .strip_prefix(CHECKPOINT_BINARY_MAGIC)
            .ok_or_else(|| StorageError::Serialization("invalid checkpoint magic".into()))?;
        let entry = bincode::deserialize(payload)
            .map_err(|e| StorageError::Serialization(format!("decode checkpoint: {e}")))?;
        Ok(Some(entry))
    }

    /// Delete a checkpoint after successful reorg rollback.
    pub fn delete_checkpoint(&self, height: u64) -> Result<()> {
        self.db.delete(&checkpoint_key(height))?;
        Ok(())
    }

    /// Load all checkpoints ordered by height ascending.
    pub fn list_checkpoints(&self) -> Result<Vec<DurableComputeCheckpoint>> {
        let mut out = Vec::new();
        self.db.for_each_prefix(CHECKPOINT_PREFIX, &mut |key, value| {
            if !key.starts_with(CHECKPOINT_PREFIX) {
                return Ok(());
            }
            let Some(payload) = value.strip_prefix(CHECKPOINT_BINARY_MAGIC) else {
                return Ok(());
            };
            if let Ok(entry) = bincode::deserialize::<DurableComputeCheckpoint>(payload) {
                out.push(entry);
            }
            Ok(())
        })?;
        out.sort_by_key(|e| e.height);
        Ok(out)
    }

    /// Saves execution result JSON by tx id.
    pub fn put_tx_result(&self, tx_id: TxId, result_json: &str) -> Result<()> {
        self.put_tx_results_batch(&[(tx_id, result_json.to_owned())])
    }

    /// Saves multiple execution results in one database batch.
    pub fn put_tx_results_batch(&self, results: &[(TxId, String)]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }

        let mut batch = self.db.batch();
        for (tx_id, result_json) in results {
            let encoded = encode_tx_result(result_json)?;
            batch.put(&tx_result_key(*tx_id), &encoded);
        }
        self.db.write_batch(batch)
    }

    /// Loads execution result JSON by tx id.
    pub fn get_tx_result(&self, tx_id: TxId) -> Result<Option<String>> {
        let Some(bytes) = self.db.get(&tx_result_key(tx_id))? else {
            return Ok(None);
        };
        decode_tx_result(&bytes).map(Some)
    }

    /// Returns whether a replay key exists (value is the unix timestamp bytes).
    pub fn has_replay_key(&self, key: &str) -> Result<Option<u64>> {
        let Some(bytes) = self.db.get(&replay_key(key))? else {
            return Ok(None);
        };
        if bytes.len() != 8 {
            return Ok(None);
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes);
        Ok(Some(u64::from_be_bytes(arr)))
    }

    /// Records a replay key used at `now_unix_secs`.
    pub fn put_replay_key(&self, key: &str, now_unix_secs: u64) -> Result<()> {
        self.db
            .put(&replay_key(key), &now_unix_secs.to_be_bytes())
    }

    /// Deletes replay keys older than the retention window.
    pub fn prune_replay_keys(&self, now_unix_secs: u64, window_secs: u64) -> Result<u64> {
        let mut stale = Vec::new();
        self.db.for_each_prefix(REPLAY_PREFIX, &mut |key, value| {
            if value.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(value);
                let ts = u64::from_be_bytes(arr);
                if now_unix_secs.saturating_sub(ts) > window_secs {
                    stale.push(key.to_vec());
                }
            } else {
                stale.push(key.to_vec());
            }
            Ok(())
        })?;
        let count = stale.len() as u64;
        if !stale.is_empty() {
            let mut batch = self.db.batch();
            for key in stale {
                batch.delete(&key);
            }
            self.db.write_batch(batch)?;
        }
        Ok(count)
    }
}

/// Durable `ReplayNonceRegistry` backed by `ComputeStore`.
pub struct PersistentReplayNonceRegistry {
    store: Arc<ComputeStore>,
    memory: parking_lot::RwLock<std::collections::HashMap<String, u64>>,
}

impl PersistentReplayNonceRegistry {
    pub fn new(store: Arc<ComputeStore>) -> Self {
        Self {
            store,
            memory: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl rabbitcore::compute::execution::ReplayNonceRegistry for PersistentReplayNonceRegistry {
    fn contains(&self, key: &str, now: u64) -> bool {
        let window = rabbitcore::compute::execution::REPLAY_NONCE_WINDOW_SECS;
        if let Some(ts) = self.memory.read().get(key).copied() {
            return now.saturating_sub(ts) <= window;
        }
        match self.store.has_replay_key(key) {
            Ok(Some(ts)) if now.saturating_sub(ts) <= window => {
                self.memory.write().insert(key.to_string(), ts);
                true
            }
            _ => false,
        }
    }

    fn insert(&self, key: String, now: u64) {
        self.memory.write().insert(key.clone(), now);
        if let Err(err) = self.store.put_replay_key(&key, now) {
            tracing::error!("failed to persist replay nonce key: {err}");
        }
    }

    fn retain_window(&self, now: u64) {
        let window = rabbitcore::compute::execution::REPLAY_NONCE_WINDOW_SECS;
        self.memory
            .write()
            .retain(|_, ts| now.saturating_sub(*ts) <= window);
        if let Err(err) = self.store.prune_replay_keys(now, window) {
            tracing::warn!("failed to prune replay nonce keys: {err}");
        }
    }
}

fn replay_key(key: &str) -> Vec<u8> {
    let mut out = REPLAY_PREFIX.to_vec();
    out.extend_from_slice(key.as_bytes());
    out
}

impl ObjectStore for ComputeStore {
    fn get_output(&self, output_id: OutputId) -> Option<ObjectOutput> {
        let key = output_key(output_id);
        let bytes = self.db.get(&key).ok().flatten()?;
        decode_output(&bytes).ok()
    }

    fn get_latest_output_by_object(&self, object_id: ObjectId) -> Option<ObjectOutput> {
        let latest_key = latest_key(object_id);
        let latest_bytes = self.db.get(&latest_key).ok().flatten()?;
        if latest_bytes.len() != 32 {
            return None;
        }
        let mut h = [0u8; 32];
        h.copy_from_slice(&latest_bytes);
        let out_id = OutputId(rabbitcore::crypto::Hash::from_bytes(h));
        self.get_output(out_id)
    }

    fn insert_output(&self, output: ObjectOutput) -> rabbitcore::compute::error::ComputeResult<()> {
        let key = output_key(output.output_id);
        if self
            .db
            .has(&key)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?
        {
            return Err(rabbitcore::compute::ComputeError::DuplicateOutputId);
        }

        let serialized = encode_output(&output)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        let latest_key = latest_key(output.object_id);

        let mut batch = self.db.batch();
        batch.put(&key, &serialized);
        batch.put(&latest_key, output.output_id.0.as_bytes());
        self.db
            .write_batch(batch)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        Ok(())
    }

    fn mark_spent(&self, output_id: OutputId) -> rabbitcore::compute::error::ComputeResult<()> {
        let Some(mut output) = self.get_output(output_id) else {
            return Err(rabbitcore::compute::ComputeError::ObjectNotFound(output_id.0));
        };
        if output.spent {
            return Err(rabbitcore::compute::ComputeError::InvalidOperation(
                "double spend detected".to_string(),
            ));
        }
        output.spent = true;
        let serialized = encode_output(&output)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        let mut batch = self.db.batch();
        batch.put(&output_key(output_id), &serialized);
        self.db
            .write_batch(batch)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        Ok(())
    }

    fn unmark_spent(&self, output_id: OutputId) -> rabbitcore::compute::error::ComputeResult<()> {
        let Some(mut output) = self.get_output(output_id) else {
            return Err(rabbitcore::compute::ComputeError::ObjectNotFound(output_id.0));
        };
        output.spent = false;
        let serialized = encode_output(&output)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        let mut batch = self.db.batch();
        batch.put(&output_key(output_id), &serialized);
        self.db
            .write_batch(batch)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        Ok(())
    }

    fn remove_output(&self, output_id: OutputId) -> rabbitcore::compute::error::ComputeResult<()> {
        let Some(existing) = self.get_output(output_id) else {
            return Err(rabbitcore::compute::ComputeError::ObjectNotFound(output_id.0));
        };
        let mut batch = self.db.batch();
        batch.delete(&output_key(output_id));
        // If this was the latest pointer for the object, clear it. A full rescan
        // of all versions is not available via prefix without extra indexes; for
        // reorg rollback of freshly created outputs this is sufficient.
        if let Some(latest) = self.get_latest_output_by_object(existing.object_id) {
            if latest.output_id == output_id {
                batch.delete(&latest_key(existing.object_id));
            }
        }
        self.db
            .write_batch(batch)
            .map_err(|e| rabbitcore::compute::ComputeError::InvalidOperation(e.to_string()))?;
        Ok(())
    }

    fn collect_outputs(&self) -> Option<Vec<ObjectOutput>> {
        let mut outs = Vec::new();
        let scan = self.db.for_each_prefix(OUTPUT_PREFIX, &mut |key, value| {
            if !key.starts_with(OUTPUT_PREFIX) {
                return Ok(());
            }
            if let Ok(output) = decode_output(value) {
                outs.push(output);
            }
            Ok(())
        });
        if scan.is_err() {
            return None;
        }
        Some(outs)
    }
}

/// Rebuilds a compute key-value database into the current on-disk format.
///
/// The target database must be distinct from the source database. Output values are decoded through
/// the backward-compatible reader and encoded with the current binary codec; all other entries are
/// copied byte-for-byte.
pub fn rebuild_compute_store(
    source: &dyn KeyValueDB,
    target: &dyn KeyValueDB,
) -> Result<ComputeRebuildStats> {
    let mut stats = ComputeRebuildStats::default();

    source.for_each_prefix(b"", &mut |key, value| {
        stats.total_entries += 1;

        if key.starts_with(OUTPUT_PREFIX) {
            stats.output_entries += 1;
            let was_binary = value.starts_with(OUTPUT_BINARY_MAGIC);
            if was_binary {
                stats.binary_outputs += 1;
            } else {
                stats.legacy_json_outputs += 1;
            }

            let output = decode_output(value)?;
            let encoded = encode_output(&output)?;
            if encoded != value {
                stats.rewritten_entries += 1;
            }
            target.put(key, &encoded)?;
        } else if key.starts_with(TX_RESULT_PREFIX) {
            stats.tx_result_entries += 1;
            let was_binary = value.starts_with(TX_RESULT_BINARY_MAGIC);
            if was_binary {
                stats.binary_tx_results += 1;
            } else {
                stats.legacy_json_tx_results += 1;
            }

            let result_json = decode_tx_result(value)?;
            let encoded = encode_tx_result(&result_json)?;
            if encoded != value {
                stats.rewritten_entries += 1;
            }
            target.put(key, &encoded)?;
        } else {
            stats.copied_entries += 1;
            target.put(key, value)?;
        }

        Ok(())
    })?;

    Ok(stats)
}

/// Prunes old non-live compute hot-state entries according to an explicit retention policy.
///
/// This never deletes unspent outputs. Archive/explorer nodes should pass `retain_all=true`.
pub fn prune_compute_store(
    db: &dyn KeyValueDB,
    config: ComputePruneConfig,
) -> Result<ComputePruneStats> {
    let mut stats = ComputePruneStats::default();
    let mut delete_keys = Vec::new();
    let mut latest_delete_keys = BTreeSet::new();

    db.for_each_prefix(b"", &mut |key, value| {
        stats.total_entries += 1;

        if key.starts_with(OUTPUT_PREFIX) {
            stats.output_entries += 1;
            let output = decode_output(value)?;
            if output.spent
                && is_past_retention(
                    output.created_at,
                    config.now_unix_secs,
                    config.retention_window_secs,
                )
            {
                stats.spent_output_candidates += 1;
                if !config.retain_all {
                    delete_keys.push(key.to_vec());
                    let latest = latest_key(output.object_id);
                    if db.get(&latest)?.as_deref() == Some(output.output_id.0.as_bytes()) {
                        latest_delete_keys.insert(latest);
                    }
                }
            }
        } else if key.starts_with(TX_RESULT_PREFIX) {
            stats.tx_result_entries += 1;
            let result_json = decode_tx_result(value)?;
            if tx_result_submitted_at_unix(&result_json)
                .map(|submitted_at| {
                    is_past_retention(
                        submitted_at,
                        config.now_unix_secs,
                        config.retention_window_secs,
                    )
                })
                .unwrap_or(false)
            {
                stats.tx_result_candidates += 1;
                if !config.retain_all {
                    delete_keys.push(key.to_vec());
                }
            }
        }

        Ok(())
    })?;

    let latest_entries_to_delete = latest_delete_keys.len() as u64;
    delete_keys.extend(latest_delete_keys);
    delete_keys.sort();
    delete_keys.dedup();

    if !config.dry_run && !config.retain_all && !delete_keys.is_empty() {
        let mut batch = db.batch();
        for key in &delete_keys {
            batch.delete(key);
        }
        db.write_batch(batch)?;
        stats.deleted_entries = delete_keys.len() as u64;
        stats.latest_entries_deleted = latest_entries_to_delete;
    }

    Ok(stats)
}

fn output_key(output_id: OutputId) -> Vec<u8> {
    let mut key = OUTPUT_PREFIX.to_vec();
    key.extend_from_slice(output_id.0.as_bytes());
    key
}

fn checkpoint_key(height: u64) -> Vec<u8> {
    let mut key = CHECKPOINT_PREFIX.to_vec();
    key.extend_from_slice(&height.to_be_bytes());
    key
}

/// Inclusion proof for one unspent output under the unspent MPT root.
#[derive(Clone, Debug)]
pub struct UnspentOutputProof {
    pub root: rabbitcore::crypto::Hash,
    pub output_id: [u8; 32],
    pub value: Vec<u8>,
    /// Encoded trie nodes from root toward the leaf (as produced by MerklePatriciaTrie::get_proof).
    pub nodes: Vec<Vec<u8>>,
}

/// Build an in-memory unspent MPT from outputs. Empty live set → `None`.
fn build_unspent_mpt(
    outputs: &[ObjectOutput],
) -> Option<(
    rabbitcore::crypto::Hash,
    std::sync::Arc<crate::trie::MerklePatriciaTrie>,
)> {
    use crate::trie::{empty_trie_root, MemTrieDB, MerklePatriciaTrie};
    use rabbitcore::crypto::Hash;
    use std::sync::Arc;

    let mut live: Vec<&ObjectOutput> = outputs.iter().filter(|o| !o.spent).collect();
    if live.is_empty() {
        return None;
    }
    live.sort_by(|a, b| a.output_id.0.as_bytes().cmp(b.output_id.0.as_bytes()));

    let db = Arc::new(MemTrieDB::new());
    let trie = Arc::new(MerklePatriciaTrie::new(db));
    for out in live {
        let mut clean = (*out).clone();
        clean.spent = false;
        let value = bincode::serialize(&clean).ok()?;
        let _ = trie.insert(out.output_id.0.as_bytes(), value);
    }
    let root = trie.root();
    if root == empty_trie_root() {
        return None;
    }
    Some((root, trie))
}

/// H6-B2.1: MPT root over live (unspent) outputs.
///
/// Key = `output_id` (32 bytes). Value = bincode of output with `spent=false`.
/// Empty set returns the zero hash (matches empty-block mining templates).
pub fn compute_unspent_mpt_root(outputs: &[ObjectOutput]) -> rabbitcore::crypto::Hash {
    use rabbitcore::crypto::Hash;
    match build_unspent_mpt(outputs) {
        Some((root, _)) => root,
        None => Hash::zero(),
    }
}

/// Generate an inclusion proof for `output_id` against the unspent MPT of `outputs`.
/// Returns `None` if the output is missing or spent.
pub fn prove_unspent_output(
    outputs: &[ObjectOutput],
    output_id: &OutputId,
) -> Option<UnspentOutputProof> {
    let out = outputs
        .iter()
        .find(|o| o.output_id == *output_id && !o.spent)?;
    let mut clean = out.clone();
    clean.spent = false;
    let value = bincode::serialize(&clean).ok()?;
    let (root, trie) = build_unspent_mpt(outputs)?;
    let proof = trie.get_proof(output_id.0.as_bytes()).ok()?;
    let mut id = [0u8; 32];
    id.copy_from_slice(output_id.0.as_bytes());
    Some(UnspentOutputProof {
        root,
        output_id: id,
        value,
        nodes: proof.nodes().to_vec(),
    })
}

/// Build an MPT from outputs using a persistent trie DB backed by `db`.
/// Nodes are written to the KV store under the `trie:node:` prefix.
/// Returns `(root, trie)` where the trie can be queried for proofs.
pub fn build_persistent_unspent_mpt(
    outputs: &[ObjectOutput],
    db: Arc<dyn crate::db::KeyValueDB>,
) -> Option<(rabbitcore::crypto::Hash, std::sync::Arc<crate::trie::MerklePatriciaTrie>)> {
    use crate::trie::{CachedTrieDB, MerklePatriciaTrie, PersistentTrieDB};
    use rabbitcore::crypto::Hash;
    use std::sync::Arc;

    let live: Vec<&ObjectOutput> = outputs.iter().filter(|o| !o.spent).collect();
    if live.is_empty() {
        return None;
    }

    let persistent_db = Arc::new(PersistentTrieDB::new(db));
    let cached_db = Arc::new(CachedTrieDB::new(persistent_db));
    let trie = Arc::new(MerklePatriciaTrie::new(cached_db));

    for out in live {
        let mut clean = (*out).clone();
        clean.spent = false;
        let value = bincode::serialize(&clean).ok()?;
        let _ = trie.insert(out.output_id.0.as_bytes(), value);
    }
    let root = trie.root();
    Some((root, trie))
}

fn latest_key(object_id: ObjectId) -> Vec<u8> {
    let mut key = LATEST_PREFIX.to_vec();
    key.extend_from_slice(object_id.0.as_bytes());
    key
}

fn tx_result_key(tx_id: TxId) -> Vec<u8> {
    let mut key = TX_RESULT_PREFIX.to_vec();
    key.extend_from_slice(tx_id.0.as_bytes());
    key
}

fn encode_output(output: &ObjectOutput) -> Result<Vec<u8>> {
    let encoded = bincode::serialize(output)
        .map_err(|e| StorageError::Serialization(format!("encode output failed: {e}")))?;
    let mut value = Vec::with_capacity(OUTPUT_BINARY_MAGIC.len() + encoded.len());
    value.extend_from_slice(OUTPUT_BINARY_MAGIC);
    value.extend_from_slice(&encoded);
    Ok(value)
}

fn decode_output(bytes: &[u8]) -> Result<ObjectOutput> {
    if let Some(payload) = bytes.strip_prefix(OUTPUT_BINARY_MAGIC) {
        return bincode::deserialize::<ObjectOutput>(payload)
            .map_err(|e| StorageError::Serialization(format!("decode binary output failed: {e}")));
    }

    serde_json::from_slice::<ObjectOutput>(bytes)
        .map_err(|e| StorageError::Serialization(format!("decode legacy json output failed: {e}")))
}

fn encode_tx_result(result_json: &str) -> Result<Vec<u8>> {
    let result = serde_json::from_str::<serde_json::Value>(result_json).map_err(|e| {
        StorageError::Serialization(format!("decode tx result json before encode failed: {e}"))
    })?;
    let envelope = ComputeResultEnvelope {
        result_json: serde_json::to_string(&result).map_err(|e| {
            StorageError::Serialization(format!("canonicalize tx result json failed: {e}"))
        })?,
    };
    let encoded = bincode::serialize(&envelope)
        .map_err(|e| StorageError::Serialization(format!("encode tx result failed: {e}")))?;
    let mut value = Vec::with_capacity(TX_RESULT_BINARY_MAGIC.len() + encoded.len());
    value.extend_from_slice(TX_RESULT_BINARY_MAGIC);
    value.extend_from_slice(&encoded);
    Ok(value)
}

fn decode_tx_result(bytes: &[u8]) -> Result<String> {
    if let Some(payload) = bytes.strip_prefix(TX_RESULT_BINARY_MAGIC) {
        let envelope = bincode::deserialize::<ComputeResultEnvelope>(payload).map_err(|e| {
            StorageError::Serialization(format!("decode binary tx result failed: {e}"))
        })?;
        return Ok(envelope.result_json);
    }

    String::from_utf8(bytes.to_vec())
        .map_err(|e| StorageError::Serialization(format!("decode legacy tx result failed: {e}")))
}

fn tx_result_submitted_at_unix(result_json: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(result_json)
        .ok()?
        .get("submitted_at_unix")?
        .as_u64()
}

fn is_past_retention(created_or_submitted_at: u64, now: u64, retention_window_secs: u64) -> bool {
    created_or_submitted_at.saturating_add(retention_window_secs) <= now
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rabbitcore::compute::{
        execution::ObjectStore,
        object::{ObjectKind, Ownership, Script},
        primitives::{DomainId, ObjectId, OutputId, TxId, Version},
    };
    use rabbitcore::crypto::Hash;

    use crate::db::{KeyValueDB, MemDatabase};

    use super::{
        decode_output, decode_tx_result, encode_output, encode_tx_result, latest_key, output_key,
        prune_compute_store, rebuild_compute_store, tx_result_key, ComputePruneConfig,
        ComputeStore, OUTPUT_BINARY_MAGIC, TX_RESULT_BINARY_MAGIC,
    };

    fn sample_output() -> rabbitcore::compute::ObjectOutput {
        rabbitcore::compute::ObjectOutput {
            output_id: OutputId(Hash::from_bytes([1; 32])),
            object_id: ObjectId(Hash::from_bytes([2; 32])),
            version: Version(1),
            domain_id: DomainId(0),
            kind: ObjectKind::State,
            owner: Ownership::Shared,
            predecessor: None,
            state: vec![1, 2, 3],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
            spent: false,
        }
    }

    #[test]
    fn compute_store_roundtrip() {
        let db = Arc::new(MemDatabase::new());
        let store = ComputeStore::new(db.clone());

        let output = sample_output();

        store.insert_output(output.clone()).unwrap();
        let got = store.get_output(output.output_id).unwrap();
        assert_eq!(got.object_id, output.object_id);
        let raw = db.get(&output_key(output.output_id)).unwrap().unwrap();
        assert!(raw.starts_with(OUTPUT_BINARY_MAGIC));

        let latest = store.get_latest_output_by_object(output.object_id).unwrap();
        assert_eq!(latest.output_id, output.output_id);

        store.mark_spent(output.output_id).unwrap();
        assert!(store.get_output(output.output_id).unwrap().spent);

        let tx_id = TxId(Hash::from_bytes([3; 32]));
        store.put_tx_result(tx_id, "{\"ok\":true}").unwrap();
        let raw_tx = db.get(&tx_result_key(tx_id)).unwrap().unwrap();
        assert!(raw_tx.starts_with(TX_RESULT_BINARY_MAGIC));
        assert_eq!(
            store.get_tx_result(tx_id).unwrap(),
            Some("{\"ok\":true}".to_string())
        );
    }

    #[test]
    fn compute_store_put_tx_results_batch_roundtrip() {
        let db = Arc::new(MemDatabase::new());
        let store = ComputeStore::new(db.clone());
        let tx_a = TxId(Hash::from_bytes([7; 32]));
        let tx_b = TxId(Hash::from_bytes([8; 32]));

        store
            .put_tx_results_batch(&[
                (tx_a, "{\"ok\":true}".to_string()),
                (tx_b, "{\"ok\":false}".to_string()),
            ])
            .unwrap();

        assert_eq!(
            store.get_tx_result(tx_a).unwrap(),
            Some("{\"ok\":true}".to_string())
        );
        assert_eq!(
            store.get_tx_result(tx_b).unwrap(),
            Some("{\"ok\":false}".to_string())
        );
    }

    #[test]
    fn compute_store_reads_legacy_json_output() {
        let db = Arc::new(MemDatabase::new());
        let store = ComputeStore::new(db.clone());
        let output = sample_output();
        let legacy = serde_json::to_vec(&output).unwrap();

        db.put(&output_key(output.output_id), &legacy).unwrap();

        let got = store.get_output(output.output_id).unwrap();
        assert_eq!(got, output);
    }

    #[test]
    fn output_binary_codec_roundtrip() {
        let output = sample_output();
        let encoded = encode_output(&output).unwrap();
        assert!(encoded.starts_with(OUTPUT_BINARY_MAGIC));
        assert_eq!(decode_output(&encoded).unwrap(), output);
    }

    #[test]
    fn compute_store_reads_legacy_json_tx_result() {
        let db = Arc::new(MemDatabase::new());
        let store = ComputeStore::new(db.clone());
        let tx_id = TxId(Hash::from_bytes([5; 32]));

        db.put(&tx_result_key(tx_id), br#"{"ok":true}"#).unwrap();

        assert_eq!(
            store.get_tx_result(tx_id).unwrap(),
            Some("{\"ok\":true}".to_string())
        );
    }

    #[test]
    fn tx_result_binary_codec_roundtrip() {
        let encoded = encode_tx_result(r#"{"ok":true,"items":[1,2]}"#).unwrap();
        assert!(encoded.starts_with(TX_RESULT_BINARY_MAGIC));
        let decoded = decode_tx_result(&encoded).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decoded).unwrap(),
            serde_json::json!({"ok": true, "items": [1, 2]})
        );
    }

    #[test]
    fn rebuild_compute_store_rewrites_legacy_outputs_and_copies_other_entries() {
        let source = MemDatabase::new();
        let target = MemDatabase::new();
        let output = sample_output();
        let legacy = serde_json::to_vec(&output).unwrap();
        let tx_id = TxId(Hash::from_bytes([4; 32]));

        source.put(&output_key(output.output_id), &legacy).unwrap();
        source
            .put(&latest_key(output.object_id), output.output_id.0.as_bytes())
            .unwrap();
        source
            .put(&tx_result_key(tx_id), br#"{"ok":true}"#)
            .unwrap();

        let stats = rebuild_compute_store(&source, &target).unwrap();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.output_entries, 1);
        assert_eq!(stats.legacy_json_outputs, 1);
        assert_eq!(stats.binary_outputs, 0);
        assert_eq!(stats.tx_result_entries, 1);
        assert_eq!(stats.legacy_json_tx_results, 1);
        assert_eq!(stats.binary_tx_results, 0);
        assert_eq!(stats.copied_entries, 1);
        assert_eq!(stats.rewritten_entries, 2);

        let raw_output = target.get(&output_key(output.output_id)).unwrap().unwrap();
        assert!(raw_output.starts_with(OUTPUT_BINARY_MAGIC));
        assert_eq!(decode_output(&raw_output).unwrap(), output);
        assert_eq!(
            target.get(&latest_key(output.object_id)).unwrap(),
            Some(output.output_id.0.as_bytes().to_vec())
        );
        assert_eq!(
            target.get(&tx_result_key(tx_id)).unwrap(),
            Some(encode_tx_result(r#"{"ok":true}"#).unwrap())
        );
    }

    #[test]
    fn prune_compute_store_deletes_only_old_spent_outputs_and_old_tx_results() {
        let db = MemDatabase::new();
        let mut old_spent = sample_output();
        old_spent.created_at = 10;
        old_spent.spent = true;
        let mut recent_spent = sample_output();
        recent_spent.output_id = OutputId(Hash::from_bytes([8; 32]));
        recent_spent.object_id = ObjectId(Hash::from_bytes([9; 32]));
        recent_spent.created_at = 105;
        recent_spent.spent = true;
        let mut old_unspent = sample_output();
        old_unspent.output_id = OutputId(Hash::from_bytes([10; 32]));
        old_unspent.object_id = ObjectId(Hash::from_bytes([11; 32]));
        old_unspent.created_at = 10;

        db.put(
            &output_key(old_spent.output_id),
            &encode_output(&old_spent).unwrap(),
        )
        .unwrap();
        db.put(
            &latest_key(old_spent.object_id),
            old_spent.output_id.0.as_bytes(),
        )
        .unwrap();
        db.put(
            &output_key(recent_spent.output_id),
            &encode_output(&recent_spent).unwrap(),
        )
        .unwrap();
        db.put(
            &output_key(old_unspent.output_id),
            &encode_output(&old_unspent).unwrap(),
        )
        .unwrap();

        let old_tx = TxId(Hash::from_bytes([12; 32]));
        let recent_tx = TxId(Hash::from_bytes([13; 32]));
        db.put(
            &tx_result_key(old_tx),
            &encode_tx_result(r#"{"ok":true,"submitted_at_unix":10}"#).unwrap(),
        )
        .unwrap();
        db.put(
            &tx_result_key(recent_tx),
            &encode_tx_result(r#"{"ok":true,"submitted_at_unix":105}"#).unwrap(),
        )
        .unwrap();

        let stats = prune_compute_store(
            &db,
            ComputePruneConfig {
                retain_all: false,
                now_unix_secs: 120,
                retention_window_secs: 20,
                dry_run: false,
            },
        )
        .unwrap();

        assert_eq!(stats.spent_output_candidates, 1);
        assert_eq!(stats.tx_result_candidates, 1);
        assert_eq!(stats.latest_entries_deleted, 1);
        assert_eq!(stats.deleted_entries, 3);
        assert_eq!(db.get(&output_key(old_spent.output_id)).unwrap(), None);
        assert_eq!(db.get(&latest_key(old_spent.object_id)).unwrap(), None);
        assert!(db
            .get(&output_key(recent_spent.output_id))
            .unwrap()
            .is_some());
        assert!(db
            .get(&output_key(old_unspent.output_id))
            .unwrap()
            .is_some());
        assert_eq!(db.get(&tx_result_key(old_tx)).unwrap(), None);
        assert!(db.get(&tx_result_key(recent_tx)).unwrap().is_some());
    }

    #[test]
    fn prune_compute_store_dry_run_and_retain_all_do_not_delete() {
        for retain_all in [false, true] {
            let db = MemDatabase::new();
            let mut output = sample_output();
            output.created_at = 10;
            output.spent = true;
            db.put(
                &output_key(output.output_id),
                &encode_output(&output).unwrap(),
            )
            .unwrap();

            let stats = prune_compute_store(
                &db,
                ComputePruneConfig {
                    retain_all,
                    now_unix_secs: 120,
                    retention_window_secs: 20,
                    dry_run: true,
                },
            )
            .unwrap();

            assert_eq!(stats.spent_output_candidates, 1);
            assert_eq!(stats.deleted_entries, 0);
            assert!(db.get(&output_key(output.output_id)).unwrap().is_some());
        }
    }

    #[test]
    fn persistent_replay_registry_survives_new_handle() {
        use rabbitcore::compute::execution::ReplayNonceRegistry;
        use super::PersistentReplayNonceRegistry;

        let db = Arc::new(MemDatabase::new());
        let store = Arc::new(ComputeStore::new(db.clone()));
        let reg = PersistentReplayNonceRegistry::new(store.clone());
        let now = 1_700_000_000u64;
        assert!(!reg.contains("actor|d:0|c:Some(1)|n:Some(1)|x:7", now));
        reg.insert("actor|d:0|c:Some(1)|n:Some(1)|x:7".into(), now);
        assert!(reg.contains("actor|d:0|c:Some(1)|n:Some(1)|x:7", now));

        // New registry instance over same DB must still see the key.
        let reg2 = PersistentReplayNonceRegistry::new(store);
        assert!(reg2.contains("actor|d:0|c:Some(1)|n:Some(1)|x:7", now + 10));
        // Outside window it expires after retain.
        reg2.retain_window(now + rabbitcore::compute::execution::REPLAY_NONCE_WINDOW_SECS + 1);
        assert!(!reg2.contains(
            "actor|d:0|c:Some(1)|n:Some(1)|x:7",
            now + rabbitcore::compute::execution::REPLAY_NONCE_WINDOW_SECS + 1
        ));
    }
}
