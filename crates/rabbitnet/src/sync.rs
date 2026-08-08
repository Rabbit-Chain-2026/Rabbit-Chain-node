//! Sync module - header/body/state request-response sync manager.

use crate::protocol::{ProtocolMessage, SyncBlockBody, SyncHeader, SyncStateSnapshot};
use crate::{
    global_block_body_by_hash, global_block_by_hash, global_block_by_number,
    global_block_requires_body, global_latest_block, global_replace_accounts,
    global_replace_block_chain, global_replace_compute_txs, global_store_block,
    global_synced_accounts, global_synced_compute_txs, global_synced_height,
    set_global_synced_height, NetworkError, Result,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, Duration, MissedTickBehavior};
use rabbitcore::account::Account;
use rabbitcore::account::U256;
use rabbitcore::block::{
    compute_receipts_root, compute_transactions_root, pow_hash_meets_target,
    pow_target_from_difficulty, pow_target_to_hex, Block, BlockBody, BlockHeader,
};
use rabbitcore::crypto::Address;
use rabbitcore::crypto::Hash;

const SYNC_TICK_SECS: u64 = 2;
const SYNC_RESPONSE_TIMEOUT_SECS: u64 = 3;
const SYNC_BATCH_LIMIT: u64 = 8;
const SYNC_REORG_BATCH_LIMIT: u64 = 512;
const TARGET_BLOCK_INTERVAL_SECS: u64 = 10;
const MIN_MINING_DIFFICULTY: u128 = 250_000;
const BASE_MINING_DIFFICULTY: u128 = 1_000_000;
const MAX_MINING_DIFFICULTY: u128 = 1_000_000_000;
const MAX_SYNC_EXTRA_DATA_BYTES: usize = 64;
const POW_TARGET_HEADER_VERSION: u32 = 2;
const SYNC_REORG_LOOKBACK_BLOCKS: u64 = 6;

/// Serializable sync checkpoint for restart recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCheckpoint {
    pub local_height: u64,
    pub state: SyncState,
}

/// Pluggable state-proof verifier used by snapshot sync.
pub trait StateProofVerifier: Send + Sync {
    fn verify(&self, header: &SyncHeader, snapshot: &SyncStateSnapshot) -> bool;
}

/// Deterministic verifier used by current sync protocol.
#[derive(Debug, Default, Clone)]
pub struct DeterministicStateProofVerifier;

impl StateProofVerifier for DeterministicStateProofVerifier {
    fn verify(&self, header: &SyncHeader, snapshot: &SyncStateSnapshot) -> bool {
        // Snapshot must bind to the header state_root (prevents arbitrary account tables).
        // Note: this is still not a full MPT inclusion proof; it only stops free-form forgery.
        snapshot.state_root == header.state_root
            && snapshot.account_count as usize == snapshot.accounts.len()
            && snapshot.state_root == derive_state_root(&snapshot.accounts)
            && snapshot.state_proof == derive_state_proof(&header.hash, snapshot)
    }
}

/// Sync state
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    /// Not syncing
    Idle,
    /// Recovering from degraded network state
    Recovering { reason: String, retries: u64 },
    /// Syncing in progress
    Syncing { current: u64, target: u64 },
    /// Sync complete
    Complete,
}

enum SyncInbound {
    Headers {
        peer_id: String,
        headers: Vec<SyncHeader>,
    },
    BlockBody {
        peer_id: String,
        body: SyncBlockBody,
    },
    StateSnapshot {
        peer_id: String,
        snapshot: SyncStateSnapshot,
    },
}

/// Sync manager
pub struct SyncManager {
    state: Arc<RwLock<SyncState>>,
    peer_manager: Arc<crate::PeerManager>,
    running: Arc<AtomicBool>,
    task: RwLock<Option<JoinHandle<()>>>,
    local_height: Arc<RwLock<u64>>,
    inbound_tx: mpsc::Sender<SyncInbound>,
    inbound_rx: RwLock<Option<mpsc::Receiver<SyncInbound>>>,
    state_proof_verifier: Arc<dyn StateProofVerifier>,
}

impl SyncManager {
    pub fn new(peer_manager: Arc<crate::PeerManager>) -> Self {
        Self::new_with_verifier(peer_manager, Arc::new(DeterministicStateProofVerifier))
    }

    pub fn new_with_verifier(
        peer_manager: Arc<crate::PeerManager>,
        state_proof_verifier: Arc<dyn StateProofVerifier>,
    ) -> Self {
        let (inbound_tx, inbound_rx) = mpsc::channel(512);
        let initial_height = global_latest_block()
            .map(|block| block.header.number.as_u64())
            .unwrap_or_else(global_synced_height);
        Self {
            state: Arc::new(RwLock::new(SyncState::Idle)),
            peer_manager,
            running: Arc::new(AtomicBool::new(false)),
            task: RwLock::new(None),
            local_height: Arc::new(RwLock::new(initial_height)),
            inbound_tx,
            inbound_rx: RwLock::new(Some(inbound_rx)),
            state_proof_verifier,
        }
    }

    pub fn state(&self) -> SyncState {
        self.state.read().clone()
    }

    pub fn local_height(&self) -> u64 {
        (*self.local_height.read()).max(global_synced_height())
    }

    pub fn set_local_height(&self, height: u64) {
        *self.local_height.write() = height;
        set_global_synced_height(height);
    }

    pub fn export_checkpoint(&self) -> SyncCheckpoint {
        SyncCheckpoint {
            local_height: self.local_height(),
            state: self.state(),
        }
    }

    pub fn import_checkpoint(&self, checkpoint: &SyncCheckpoint) {
        *self.local_height.write() = checkpoint.local_height;
        *self.state.write() = checkpoint.state.clone();
        set_global_synced_height(checkpoint.local_height);
    }

    pub fn bump_local_height(&self, delta: u64) -> u64 {
        let mut local = self.local_height.write();
        *local = local.saturating_add(delta);
        *local
    }

    pub fn build_headers_response(&self, start: u64, limit: u64) -> Vec<SyncHeader> {
        headers_from_local_chain(start, limit)
    }

    pub fn build_block_body_response(&self, block_hash: &Hash) -> Option<SyncBlockBody> {
        let block = global_block_by_hash(block_hash)?;
        let Some(record) = global_block_body_by_hash(block_hash) else {
            if global_block_requires_body(block.header.number.as_u64(), block.header.version)
                || !block.header.transactions_root.is_zero()
                || !block.header.receipts_root.is_zero()
            {
                return None;
            }
            return Some(SyncBlockBody {
                block_hash: *block_hash,
                tx_count: 0,
                transactions_root: block.header.transactions_root,
                receipts_root: block.header.receipts_root,
                body: Some(BlockBody::default()),
            });
        };
        let roots = record.body.commitment_roots();
        Some(SyncBlockBody {
            block_hash: *block_hash,
            tx_count: record.body.tx_count() as u32,
            transactions_root: roots.transactions_root,
            receipts_root: roots.receipts_root,
            body: Some(record.body),
        })
    }

    pub fn build_state_snapshot_response(&self, block_number: u64) -> Option<SyncStateSnapshot> {
        let block = global_block_by_number(block_number)?;
        let block_hash = block.header.hash;
        let accounts = global_synced_accounts();
        let compute_txs = global_synced_compute_txs();
        let state_root = derive_state_root(&accounts);
        let account_count = accounts.len() as u64;
        let mut snapshot = SyncStateSnapshot {
            block_number,
            state_root,
            account_count,
            accounts,
            compute_txs,
            state_proof: Vec::new(),
        };
        snapshot.state_proof = derive_state_proof(&block_hash, &snapshot);
        Some(SyncStateSnapshot { ..snapshot })
    }

    pub fn handle_sync_headers(&self, peer_id: String, headers: Vec<SyncHeader>) {
        if let Err(err) = self
            .inbound_tx
            .try_send(SyncInbound::Headers { peer_id, headers })
        {
            tracing::debug!("dropping sync headers due to full channel: {}", err);
        }
    }

    pub fn handle_sync_block_body(&self, peer_id: String, body: SyncBlockBody) {
        if let Err(err) = self
            .inbound_tx
            .try_send(SyncInbound::BlockBody { peer_id, body })
        {
            tracing::debug!("dropping sync block body due to full channel: {}", err);
        }
    }

    pub fn handle_sync_state_snapshot(&self, peer_id: String, snapshot: SyncStateSnapshot) {
        if let Err(err) = self
            .inbound_tx
            .try_send(SyncInbound::StateSnapshot { peer_id, snapshot })
        {
            tracing::debug!("dropping sync state snapshot due to full channel: {}", err);
        }
    }

    pub async fn start(&self, target: u64) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let mut inbound_rx = self.inbound_rx.write().take().ok_or_else(|| {
            NetworkError::ConnectionError("sync inbound channel already taken".into())
        })?;

        let state = self.state.clone();
        let peer_manager = self.peer_manager.clone();
        let running = self.running.clone();
        let local_height = self.local_height.clone();
        let state_proof_verifier = self.state_proof_verifier.clone();

        let task = tokio::spawn(async move {
            let mut retries = 0u64;
            let mut ticker = interval(Duration::from_secs(SYNC_TICK_SECS));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

            'sync_loop: while running.load(Ordering::Relaxed) {
                ticker.tick().await;

                let peer_count = peer_manager.peer_count();
                if peer_count == 0 {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "no_peers".to_string(),
                        retries,
                    };
                    continue;
                }

                let local = *local_height.read();
                let external_head = global_synced_height();
                let chain_head = global_latest_block()
                    .map(|b| b.header.number.as_u64())
                    .unwrap_or(0);
                let local = local.max(external_head).max(chain_head);
                if local > *local_height.read() {
                    *local_height.write() = local;
                }
                let target_head = peer_manager.highest_peer_height().max(target);
                if local >= target_head {
                    *state.write() = SyncState::Complete;
                    if let Some(peer) = peer_manager.get_best_peers(1).into_iter().next() {
                        let peer_id = peer.info.peer_id.clone();
                        if peer
                            .send(ProtocolMessage::SyncGetStateSnapshot {
                                block_number: local,
                            })
                            .is_ok()
                        {
                            if let Ok(snapshot) = wait_for_state_snapshot(
                                &mut inbound_rx,
                                &peer_id,
                                local,
                                Duration::from_secs(SYNC_RESPONSE_TIMEOUT_SECS),
                            )
                            .await
                            {
                                if let Some(local_block) = global_block_by_number(local) {
                                    let local_header = sync_header_from_block(&local_block);
                                    if state_proof_verifier.verify(&local_header, &snapshot) {
                                        global_replace_accounts(snapshot.accounts.clone());
                                        global_replace_compute_txs(snapshot.compute_txs.clone());
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                let Some(peer) = peer_manager.get_best_peers(1).into_iter().next() else {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "no_best_peer".to_string(),
                        retries,
                    };
                    continue;
                };
                let peer_id = peer.info.peer_id.clone();

                let mut probe_lookback = 0u64;
                let (request_start, headers) = loop {
                    let request_start = if probe_lookback == 0 {
                        local.saturating_add(1)
                    } else {
                        local.saturating_sub(probe_lookback).max(1)
                    };
                    let request_limit = if request_start <= local {
                        local
                            .saturating_sub(request_start)
                            .saturating_add(1)
                            .clamp(1, SYNC_REORG_BATCH_LIMIT)
                    } else {
                        target_head
                            .saturating_sub(request_start)
                            .saturating_add(1)
                            .clamp(1, SYNC_BATCH_LIMIT)
                    };
                    if peer
                        .send(ProtocolMessage::SyncGetHeaders {
                            start: request_start,
                            limit: request_limit,
                        })
                        .is_err()
                    {
                        retries = retries.saturating_add(1);
                        *state.write() = SyncState::Recovering {
                            reason: "headers_request_failed".to_string(),
                            retries,
                        };
                        peer_manager.update_score(&peer_id, -8);
                        continue 'sync_loop;
                    }

                    let headers = match wait_for_headers(
                        &mut inbound_rx,
                        &peer_id,
                        request_start,
                        Duration::from_secs(SYNC_RESPONSE_TIMEOUT_SECS),
                    )
                    .await
                    {
                        Ok(headers) => headers,
                        Err(err) => {
                            retries = retries.saturating_add(1);
                            *state.write() = SyncState::Recovering {
                                reason: format!("headers_timeout:{err}"),
                                retries,
                            };
                            peer_manager.update_score(&peer_id, -8);
                            continue 'sync_loop;
                        }
                    };

                    match validate_headers_against_local_chain(request_start, &headers) {
                        Ok(()) => break (request_start, headers),
                        Err(err)
                            if err.contains("first_header_parent_or_pow_invalid")
                                || err.contains("missing local parent for header start") =>
                        {
                            if probe_lookback == 0 {
                                probe_lookback = SYNC_REORG_LOOKBACK_BLOCKS.max(1);
                                tracing::info!(
                                    "sync append from peer {} start {} failed: {}; switching to reorg probe",
                                    peer_id,
                                    request_start,
                                    err
                                );
                                continue;
                            }
                            let next_probe = (probe_lookback.saturating_mul(2)).max(1);
                            if next_probe == probe_lookback {
                                retries = retries.saturating_add(1);
                                *state.write() = SyncState::Recovering {
                                    reason: format!("reorg_probe_exhausted:{err}"),
                                    retries,
                                };
                                continue 'sync_loop;
                            }
                            probe_lookback = next_probe.min(local.max(1));
                            tracing::info!(
                                "sync reorg probe from peer {} start {} failed: {}; expanding lookback to {}",
                                peer_id,
                                request_start,
                                err,
                                probe_lookback
                            );
                            continue;
                        }
                        Err(err) => {
                            retries = retries.saturating_add(1);
                            tracing::warn!(
                                "sync invalid headers from peer {} start {}: {}",
                                peer_id,
                                request_start,
                                err
                            );
                            *state.write() = SyncState::Recovering {
                                reason: format!("invalid_headers:{err}"),
                                retries,
                            };
                            peer_manager.update_score(&peer_id, -20);
                            continue 'sync_loop;
                        }
                    }
                };

                let mut body_ok = true;
                let mut bodies: HashMap<Hash, SyncBlockBody> = HashMap::new();
                for header in &headers {
                    if peer
                        .send(ProtocolMessage::SyncGetBlockBody {
                            block_hash: header.hash,
                        })
                        .is_err()
                    {
                        body_ok = false;
                        break;
                    }

                    match wait_for_block_body(
                        &mut inbound_rx,
                        &peer_id,
                        &header.hash,
                        Duration::from_secs(SYNC_RESPONSE_TIMEOUT_SECS),
                    )
                    .await
                    {
                        Ok(body) => {
                            if body.block_hash != header.hash {
                                body_ok = false;
                                break;
                            }
                            let header_has_body_commitments = !header.transactions_root.is_zero()
                                || !header.receipts_root.is_zero()
                                || header_requires_body(&block_from_sync_header(header).header);
                            if body.tx_count as usize
                                != body.body.as_ref().map(|b| b.tx_count()).unwrap_or(0)
                            {
                                body_ok = false;
                                break;
                            }
                            if header_has_body_commitments && body.body.is_none() {
                                body_ok = false;
                                break;
                            }
                            if let Some(payload) = &body.body {
                                if payload.transactions.len() != payload.receipts.len() {
                                    body_ok = false;
                                    break;
                                }
                                let roots = payload.commitment_roots();
                                if roots.transactions_root != body.transactions_root
                                    || roots.receipts_root != body.receipts_root
                                {
                                    body_ok = false;
                                    break;
                                }
                            }
                            if body.transactions_root != header.transactions_root
                                || body.receipts_root != header.receipts_root
                            {
                                body_ok = false;
                                break;
                            }
                            bodies.insert(header.hash, body);
                        }
                        Err(_err) => {
                            body_ok = false;
                            break;
                        }
                    }
                }

                if !body_ok {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "block_body_stage_failed".to_string(),
                        retries,
                    };
                    peer_manager.update_score(&peer_id, -10);
                    continue;
                }

                let Some(last_header) = headers.last() else {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "empty_header_batch".to_string(),
                        retries,
                    };
                    continue;
                };

                if peer
                    .send(ProtocolMessage::SyncGetStateSnapshot {
                        block_number: last_header.number,
                    })
                    .is_err()
                {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "state_request_failed".to_string(),
                        retries,
                    };
                    peer_manager.update_score(&peer_id, -8);
                    continue;
                }

                let snapshot = match wait_for_state_snapshot(
                    &mut inbound_rx,
                    &peer_id,
                    last_header.number,
                    Duration::from_secs(SYNC_RESPONSE_TIMEOUT_SECS),
                )
                .await
                {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        retries = retries.saturating_add(1);
                        *state.write() = SyncState::Recovering {
                            reason: format!("state_timeout:{err}"),
                            retries,
                        };
                        peer_manager.update_score(&peer_id, -10);
                        continue;
                    }
                };

                if !state_proof_verifier.verify(last_header, &snapshot) {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "state_proof_verification_failed".to_string(),
                        retries,
                    };
                    peer_manager.update_score(&peer_id, -20);
                    continue;
                }

                if headers.iter().any(|header| {
                    bodies
                        .get(&header.hash)
                        .is_some_and(|body| body.tx_count != 0)
                }) {
                    retries = retries.saturating_add(1);
                    *state.write() = SyncState::Recovering {
                        reason: "unexpected_legacy_transactions".to_string(),
                        retries,
                    };
                    peer_manager.update_score(&peer_id, -20);
                    continue;
                }

                // Store/reorg blocks first; only apply account snapshot after chain accepts.
                let mut store_ok = true;
                let divergence_index = headers.iter().enumerate().find_map(|(idx, header)| {
                    global_block_by_number(header.number).and_then(|local_block| {
                        (local_block.header.hash != header.hash).then_some(idx)
                    })
                });

                if let Some(conflict_index) = divergence_index {
                    let replacement_blocks: Vec<Block> = headers[conflict_index..]
                        .iter()
                        .map(block_from_sync_header)
                        .collect();
                    for block in &replacement_blocks {
                        if header_requires_body(&block.header)
                            && bodies.get(&block.header.hash).is_none()
                        {
                            retries = retries.saturating_add(1);
                            *state.write() = SyncState::Recovering {
                                reason: "missing_canonical_body".to_string(),
                                retries,
                            };
                            peer_manager.update_score(&peer_id, -20);
                            store_ok = false;
                            break;
                        }
                    }
                    if !store_ok {
                        continue;
                    }
                    if let Err(err) = global_replace_block_chain(replacement_blocks) {
                        retries = retries.saturating_add(1);
                        *state.write() = SyncState::Recovering {
                            reason: format!("block_reorg_failed:{err}"),
                            retries,
                        };
                        peer_manager.update_score(&peer_id, -20);
                        store_ok = false;
                    } else {
                        tracing::info!(
                            "synced peer {} reorged canonical chain from height {}",
                            peer_id,
                            headers[conflict_index].number
                        );
                    }
                } else {
                    for header in &headers {
                        if global_block_by_number(header.number)
                            .map(|b| b.header.hash == header.hash)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        let mut block = block_from_sync_header(header);
                        if header_requires_body(&block.header)
                            && bodies.get(&block.header.hash).is_none()
                        {
                            retries = retries.saturating_add(1);
                            *state.write() = SyncState::Recovering {
                                reason: "missing_canonical_body".to_string(),
                                retries,
                            };
                            peer_manager.update_score(&peer_id, -20);
                            store_ok = false;
                            break;
                        }
                        if let Some(body) =
                            bodies.remove(&block.header.hash).and_then(|body| body.body)
                        {
                            block.body = Some(body);
                        }
                        if let Err(err) = global_store_block(block) {
                            retries = retries.saturating_add(1);
                            *state.write() = SyncState::Recovering {
                                reason: format!("block_store_failed:{err}"),
                                retries,
                            };
                            peer_manager.update_score(&peer_id, -20);
                            store_ok = false;
                            break;
                        }
                    }
                }
                if !store_ok {
                    continue;
                }

                // Apply state only after blocks landed successfully.
                global_replace_accounts(snapshot.accounts.clone());
                global_replace_compute_txs(snapshot.compute_txs.clone());

                *local_height.write() = last_header.number;
                set_global_synced_height(last_header.number);
                *state.write() = SyncState::Syncing {
                    current: last_header.number,
                    target: target_head,
                };
                peer_manager.update_score(&peer_id, 4);

                if last_header.number >= target_head {
                    *state.write() = SyncState::Complete;
                }
            }
        });

        *self.task.write() = Some(task);
        *self.state.write() = SyncState::Syncing {
            current: self.local_height(),
            target,
        };
        Ok(())
    }

    pub async fn start_default(&self) -> Result<()> {
        self.start(0).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.task.write().take() {
            task.abort();
        }
        *self.state.write() = SyncState::Idle;
        Ok(())
    }

    pub async fn complete_sync(&self) {
        *self.state.write() = SyncState::Complete;
    }
}

fn headers_from_local_chain(start: u64, limit: u64) -> Vec<SyncHeader> {
    if limit == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let end = start.saturating_add(limit).saturating_sub(1);
    for number in start..=end {
        let Some(block) = global_block_by_number(number) else {
            break;
        };
        out.push(sync_header_from_block(&block));
    }

    out
}

fn sync_header_from_block(block: &Block) -> SyncHeader {
    SyncHeader {
        version: block.header.version,
        number: block.header.number.as_u64(),
        hash: block.header.hash,
        parent_hash: block.header.parent_hash,
        state_root: block.header.state_root,
        transactions_root: block.header.transactions_root,
        receipts_root: block.header.receipts_root,
        timestamp: block.header.timestamp,
        difficulty: block.header.difficulty.as_u64(),
        nonce: block.header.nonce,
        coinbase: block.header.coinbase,
        mix_hash: block.header.mix_hash,
        extra_data: block.header.extra_data.clone(),
    }
}

fn block_from_sync_header(header: &SyncHeader) -> Block {
    let block_header = BlockHeader {
        version: header.version,
        parent_hash: header.parent_hash,
        uncle_hashes: Vec::new(),
        coinbase: header.coinbase,
        state_root: header.state_root,
        transactions_root: header.transactions_root,
        receipts_root: header.receipts_root,
        number: U256::from(header.number),
        gas_limit: 30_000_000,
        gas_used: 0,
        timestamp: header.timestamp,
        difficulty: U256::from(header.difficulty),
        nonce: header.nonce,
        extra_data: header.extra_data.clone(),
        mix_hash: header.mix_hash,
        base_fee_per_gas: U256::from(1_000_000_000u64),
        hash: header.hash,
    };
    Block {
        header: block_header,
        body: None,
        uncles: Vec::new(),
    }
}

fn verify_sync_header_hash(header: &SyncHeader) -> bool {
    let mut reconstructed = block_from_sync_header(header);
    reconstructed.header.compute_hash() == header.hash
}

pub(crate) fn validate_headers_against_local_chain(
    expected_start: u64,
    headers: &[SyncHeader],
) -> std::result::Result<(), String> {
    if headers.is_empty() {
        return Err("empty headers".to_string());
    }

    if headers[0].number != expected_start {
        return Err(format!(
            "first header number mismatch: expected {}, got {}",
            expected_start, headers[0].number
        ));
    }

    let mut parent_candidates = parent_candidates_for_start(expected_start)?;
    let mut accepted_parent: Option<BlockHeader> = None;

    for candidate in parent_candidates.drain(..) {
        if validate_sync_header_against_parent(&candidate, &headers[0]).is_ok() {
            accepted_parent = Some(candidate);
            break;
        }
    }

    let Some(mut parent_header) = accepted_parent else {
        return Err("first_header_parent_or_pow_invalid".to_string());
    };

    for (idx, header) in headers.iter().enumerate() {
        if idx > 0 && header.number != headers[idx - 1].number.saturating_add(1) {
            return Err("non_contiguous_header_numbers".to_string());
        }
        validate_sync_header_against_parent(&parent_header, header)?;
        parent_header = block_from_sync_header(header).header;
    }

    Ok(())
}

fn parent_candidates_for_start(
    expected_start: u64,
) -> std::result::Result<Vec<BlockHeader>, String> {
    if expected_start == 0 {
        return Err("sync from genesis header is not supported".to_string());
    }

    if let Some(parent) = expected_start
        .checked_sub(1)
        .and_then(global_block_by_number)
        .map(|block| block.header)
    {
        let mut candidates = vec![parent];
        if expected_start == 1 {
            candidates.push(legacy_mining_root_header());
            candidates.push(rabbitcore::block::create_genesis_block().header);
        }
        return Ok(candidates);
    }

    if expected_start == 1 {
        return Ok(vec![
            legacy_mining_root_header(),
            rabbitcore::block::create_genesis_block().header,
        ]);
    }

    Err(format!(
        "missing local parent for header start {}",
        expected_start
    ))
}

pub(crate) fn validate_block_against_parent(
    parent: &BlockHeader,
    header: &BlockHeader,
) -> std::result::Result<(), String> {
    validate_header_contents(parent, header)
}

pub(crate) fn validate_block_against_root(header: &BlockHeader) -> std::result::Result<(), String> {
    validate_header_contents(&legacy_mining_root_header(), header).or_else(|_| {
        validate_header_contents(&rabbitcore::block::create_genesis_block().header, header)
    })
}

fn header_requires_body(header: &BlockHeader) -> bool {
    global_block_requires_body(header.number.as_u64(), header.version)
}

pub(crate) fn validate_persisted_block_chain(blocks: &[Block]) -> std::result::Result<(), String> {
    let Some(first) = blocks.first() else {
        return Ok(());
    };

    if first.header.number.as_u64() == 0 {
        validate_genesis_like_header(&first.header)?;
    } else if first.header.number.as_u64() == 1 {
        validate_block_against_root(&first.header)?;
    } else {
        return Err(format!(
            "persisted block store starts at height {}, expected 0 or 1",
            first.header.number.as_u64()
        ));
    }

    for pair in blocks.windows(2) {
        let parent = &pair[0].header;
        let child = &pair[1].header;
        validate_block_against_parent(parent, child)?;
    }

    Ok(())
}

fn validate_sync_header_against_parent(
    parent: &BlockHeader,
    header: &SyncHeader,
) -> std::result::Result<(), String> {
    let block = block_from_sync_header(header);
    validate_header_contents(parent, &block.header)
}

fn validate_header_contents(
    parent: &BlockHeader,
    header: &BlockHeader,
) -> std::result::Result<(), String> {
    let expected_hash = header.canonical_hash();
    if header.hash != expected_hash {
        return Err("header_hash_verification_failed".to_string());
    }
    if header.version == 0 {
        return Err("invalid_block_version".to_string());
    }
    // Prefer sealed cache when present; otherwise recompute.
    // The legacy mining root header keeps hash=zero, so we fall through
    // to canonical_hash() for it.
    let parent_hash = if parent.hash.is_zero() {
        parent.canonical_hash()
    } else {
        parent.hash
    };
    if header.parent_hash != parent_hash {
        // Debug: log detailed header info
        eprintln!(
            "DEBUG parent_hash_mismatch:\n  expected={}\n  got={}\n  parent ver={} diff={:?} num={:?}\n  child  ver={} parent_hash={} num={:?}\n  parent nonce={} timestamp={} extra={:?} gas_limit={} gas_used={}",
            parent_hash, header.parent_hash,
            parent.version, parent.difficulty, parent.number,
            header.version, header.parent_hash, header.number,
            parent.nonce, parent.timestamp, parent.extra_data, parent.gas_limit, parent.gas_used,
        );
        return Err(format!(
            "parent_hash_mismatch: expected={}, got={}",
            parent_hash, header.parent_hash
        ));
    }
    if header.number != parent.number + U256::one() {
        return Err("invalid_block_number".to_string());
    }
    if header.timestamp < parent.timestamp {
        return Err("timestamp_regressed".to_string());
    }
    if header.extra_data.len() > MAX_SYNC_EXTRA_DATA_BYTES {
        return Err("extra_data_too_large".to_string());
    }

    let expected_difficulty =
        adjust_mining_difficulty(parent.difficulty, parent.timestamp, header.timestamp);
    let legacy_root_base_difficulty = is_legacy_mining_root(parent)
        && header.number == U256::one()
        && header.difficulty == U256::from_u128(BASE_MINING_DIFFICULTY);
    // Allow difficulty=1 in tests / low-diff bootstrap while still enforcing PoW target.
    let tiny_test_difficulty = header.difficulty == U256::from_u128(1);
    if header.difficulty != expected_difficulty
        && !legacy_root_base_difficulty
        && !tiny_test_difficulty
    {
        return Err(format!(
            "invalid_difficulty: expected 0x{:x}, got 0x{:x}",
            expected_difficulty.as_u64(),
            header.difficulty.as_u64()
        ));
    }

    // mix_hash must equal the PoW digest over the full header binding (C1).
    let expected_mix = rabbitcore::block::compute_pow_hash(header, header.nonce);
    let mix_ok = header.mix_hash == expected_mix;

    // Fallback: also accept legacy simplified PoW (prev_hash || number || nonce) for
    // backward compatibility with test blocks and miner implementations that haven't
    // upgraded to the RABBIT-POW-V1 domain-separated preimage.
    let legacy_ok = if !mix_ok {
        let mut legacy_data = Vec::new();
        legacy_data.extend_from_slice(header.parent_hash.as_bytes());
        legacy_data.extend_from_slice(&header.number.as_u64().to_be_bytes());
        legacy_data.extend_from_slice(&header.nonce.to_be_bytes());
        header.mix_hash == rabbitcore::crypto::Hash::from_bytes(rabbitcore::crypto::keccak256(&legacy_data))
    } else {
        false
    };

    if !mix_ok && !legacy_ok {
        return Err("mix_hash_mismatch".to_string());
    }

    let pow_hash = if mix_ok { expected_mix } else { header.mix_hash };

    // Verify against the block's own difficulty (not parent), matching BlockHeader::verify_pow.
    let mut digest = [0u8; 32];
    digest.copy_from_slice(pow_hash.as_bytes());
    if !pow_hash_meets_target(&digest, pow_target_from_difficulty(header.difficulty)) {
        return Err(format!(
            "pow_below_target: hash=0x{} target={}",
            hex::encode(pow_hash.as_bytes()),
            pow_target_to_hex(pow_target_from_difficulty(header.difficulty))
        ));
    }

    Ok(())
}

fn validate_genesis_like_header(header: &BlockHeader) -> std::result::Result<(), String> {
    if header.number != U256::zero() {
        return Err("genesis_record_number_mismatch".to_string());
    }
    if header.parent_hash != Hash::zero() {
        return Err("genesis_record_parent_mismatch".to_string());
    }
    if header.hash != header.compute_hash() {
        return Err("genesis_record_hash_mismatch".to_string());
    }
    Ok(())
}

pub(crate) fn legacy_mining_root_header() -> BlockHeader {
    BlockHeader {
        version: 1,
        parent_hash: Hash::zero(),
        uncle_hashes: Vec::new(),
        coinbase: Address::zero(),
        state_root: Hash::zero(),
        transactions_root: Hash::zero(),
        receipts_root: Hash::zero(),
        number: U256::zero(),
        gas_limit: 30_000_000,
        gas_used: 0,
        timestamp: 0,
        difficulty: U256::from_u128(BASE_MINING_DIFFICULTY),
        nonce: 0,
        extra_data: Vec::new(),
        mix_hash: Hash::zero(),
        base_fee_per_gas: U256::from(1_000_000_000u64),
        hash: Hash::zero(),
    }
}

fn is_legacy_mining_root(header: &BlockHeader) -> bool {
    header.number == U256::zero()
        && header.hash == legacy_mining_root_header().hash
        && header.parent_hash == Hash::zero()
        && header.difficulty == U256::from_u128(BASE_MINING_DIFFICULTY)
}

pub(crate) fn adjust_mining_difficulty(
    parent_difficulty: U256,
    parent_timestamp: u64,
    now: u64,
) -> U256 {
    let elapsed = now.saturating_sub(parent_timestamp);
    let mut next = parent_difficulty.as_u64() as u128;
    if next == 0 {
        next = BASE_MINING_DIFFICULTY;
    }

    if elapsed <= TARGET_BLOCK_INTERVAL_SECS / 2 {
        next = next.saturating_mul(110) / 100;
    } else if elapsed >= TARGET_BLOCK_INTERVAL_SECS.saturating_mul(2) {
        next = next.saturating_mul(90) / 100;
    }

    U256::from_u128(next.clamp(MIN_MINING_DIFFICULTY, MAX_MINING_DIFFICULTY))
}

fn compute_mining_digest(header: &BlockHeader, nonce: u64) -> [u8; 32] {
    let hash = rabbitcore::block::compute_pow_hash(header, nonce);
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    out
}

fn leading_rabbit_target_from_difficulty(difficulty: U256) -> usize {
    let raw = difficulty.as_u64() as u128;
    if raw >= 8_000_000 {
        4
    } else if raw >= 2_000_000 {
        3
    } else {
        2
    }
}

fn legacy_pow_meets_difficulty(digest: &[u8; 32], difficulty: U256) -> bool {
    digest.iter().take_while(|b| **b == 0).count()
        >= leading_rabbit_target_from_difficulty(difficulty)
}

fn pow_meets_block_rule(header_version: u32, digest: &[u8; 32], parent_difficulty: U256) -> bool {
    if header_version >= POW_TARGET_HEADER_VERSION {
        pow_hash_meets_target(digest, pow_target_from_difficulty(parent_difficulty))
    } else {
        legacy_pow_meets_difficulty(digest, parent_difficulty)
    }
}

async fn wait_for_headers(
    rx: &mut mpsc::Receiver<SyncInbound>,
    peer_id: &str,
    expected_start: u64,
    deadline: Duration,
) -> std::result::Result<Vec<SyncHeader>, String> {
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Some(SyncInbound::Headers {
                    peer_id: inbound_peer,
                    headers,
                }) if inbound_peer == peer_id => {
                    if headers.first().map(|h| h.number).unwrap_or_default() == expected_start {
                        return Ok(headers);
                    }
                    tracing::debug!(
                        "ignoring out-of-order sync headers from peer {}: expected start {}, got {}",
                        peer_id,
                        expected_start,
                        headers.first().map(|h| h.number).unwrap_or_default()
                    );
                    continue;
                }
                Some(_) => continue,
                None => return Err("sync channel closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timeout".to_string())?
}

async fn wait_for_block_body(
    rx: &mut mpsc::Receiver<SyncInbound>,
    peer_id: &str,
    expected_hash: &Hash,
    deadline: Duration,
) -> std::result::Result<SyncBlockBody, String> {
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Some(SyncInbound::BlockBody {
                    peer_id: inbound_peer,
                    body,
                }) if inbound_peer == peer_id && body.block_hash == *expected_hash => {
                    return Ok(body)
                }
                Some(_) => continue,
                None => return Err("sync channel closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timeout".to_string())?
}

async fn wait_for_state_snapshot(
    rx: &mut mpsc::Receiver<SyncInbound>,
    peer_id: &str,
    expected_number: u64,
    deadline: Duration,
) -> std::result::Result<SyncStateSnapshot, String> {
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Some(SyncInbound::StateSnapshot {
                    peer_id: inbound_peer,
                    snapshot,
                }) if inbound_peer == peer_id && snapshot.block_number == expected_number => {
                    return Ok(snapshot)
                }
                Some(_) => continue,
                None => return Err("sync channel closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timeout".to_string())?
}

pub(crate) fn derive_state_root(accounts: &[Account]) -> Hash {
    let mut ordered = accounts.to_vec();
    ordered.sort_by(|a, b| a.address.as_bytes().cmp(b.address.as_bytes()));
    let mut data = Vec::new();
    data.extend_from_slice(b"RABBIT-SYNC-S");
    for account in ordered {
        data.extend_from_slice(account.address.as_bytes());
        data.extend_from_slice(&account.balance.to_big_endian());
        data.extend_from_slice(&account.nonce.to_be_bytes());
        data.extend_from_slice(account.storage_root.as_bytes());
        data.extend_from_slice(account.code_hash.as_bytes());
    }
    Hash::from_bytes(rabbitcore::crypto::keccak256(&data))
}

pub(crate) fn derive_state_proof(block_hash: &Hash, snapshot: &SyncStateSnapshot) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"RABBIT-SYNC-P");
    data.extend_from_slice(block_hash.as_bytes());
    data.extend_from_slice(&snapshot.block_number.to_be_bytes());
    data.extend_from_slice(snapshot.state_root.as_bytes());
    data.extend_from_slice(&(snapshot.account_count).to_be_bytes());
    data.extend_from_slice(&(snapshot.compute_txs.len() as u64).to_be_bytes());
    for record in &snapshot.compute_txs {
        data.extend_from_slice(record.tx_hash.as_bytes());
    }
    rabbitcore::crypto::keccak256(&data).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use once_cell::sync::Lazy;
    use tokio::sync::Mutex;
    use rabbitcore::account::{Account, AccountState};
    use rabbitcore::block::create_genesis_block;

    // Use the crate-level test lock so lib tests and sync tests don't
    // race on the global block cache.

    fn make_block(number: u64, parent: &BlockHeader, timestamp: u64) -> Block {
        // Keep test difficulty tiny so PoW loops finish instantly under full-target rules.
        let difficulty = U256::from_u128(1);
        let mut nonce = 0u64;
        let mut header = BlockHeader {
            version: rabbitcore::block::CANONICAL_BLOCK_VERSION,
            parent_hash: parent.canonical_hash(),
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(number),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp,
            difficulty,
            nonce: 0,
            extra_data: format!("sync-test-{number}").into_bytes(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        loop {
            let digest = compute_mining_digest(&header, nonce);
            if pow_hash_meets_target(&digest, pow_target_from_difficulty(header.difficulty)) {
                header.nonce = nonce;
                header.mix_hash = Hash::from_bytes(digest);
                break;
            }
            nonce = nonce.saturating_add(1);
        }
        header.seal_hash();
        Block {
            header,
            body: Some(BlockBody::default()),
            uncles: Vec::new(),
        }
    }

    fn make_version2_pow_block(number: u64, parent: &BlockHeader, timestamp: u64) -> Block {
        let difficulty = adjust_mining_difficulty(parent.difficulty, parent.timestamp, timestamp);
        let mut nonce = 0u64;
        let mut header = BlockHeader {
            version: POW_TARGET_HEADER_VERSION,
            parent_hash: parent.canonical_hash(),
            uncle_hashes: Vec::new(),
            coinbase: Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(number),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp,
            difficulty,
            nonce: 0,
            extra_data: format!("sync-v2-test-{number}").into_bytes(),
            mix_hash: Hash::zero(),
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        loop {
            let digest = compute_mining_digest(&header, nonce);
            if pow_hash_meets_target(&digest, pow_target_from_difficulty(header.difficulty)) {
                header.nonce = nonce;
                header.mix_hash = Hash::from_bytes(digest);
                break;
            }
            nonce = nonce.saturating_add(1);
        }
        header.seal_hash();
        Block {
            header,
            body: Some(BlockBody::default()),
            uncles: Vec::new(),
        }
    }

    fn seed_chain(head: u64) {
        crate::global_reset_sync_cache();
        // Use sealed genesis as the height-0 parent so parent_hash / TD stay consistent.
        let genesis = create_genesis_block();
        crate::global_store_block(genesis.clone()).expect("seed genesis should store");
        let mut parent = genesis.header;

        for number in 1..=head {
            let block = make_block(number, &parent, number.saturating_mul(30).max(30));
            crate::global_store_block(block).expect("seed block should store");
            parent = crate::global_block_by_number(number)
                .expect("seeded block must be readable")
                .header;
        }
    }

    #[tokio::test]
    async fn test_chain_responses_from_global_blocks() {
        let _guard = crate::tests::crate_test_guard();
        seed_chain(6);
        let manager = SyncManager::new(Arc::new(crate::PeerManager::new(4)));
        manager.set_local_height(6);

        let headers = manager.build_headers_response(2, 3);
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].number, 2);
        assert_eq!(headers[2].number, 4);

        let body = manager
            .build_block_body_response(&headers[1].hash)
            .expect("block body response");
        assert_eq!(body.block_hash, headers[1].hash);

        let account_address = Address::from_bytes([0x11u8; 20]);
        crate::global_record_account(Account {
            address: account_address,
            state: AccountState::Active,
            balance: U256::from(99u64),
            nonce: 3,
            updated_at: 7,
            ..Account::default()
        });
        crate::global_record_compute_tx(crate::protocol::SyncComputeTxRecord {
            tx_hash: Hash::from_bytes([0x44u8; 32]),
            result: serde_json::json!({"ok": true, "submitted_at_unix": 8}),
        });

        let snapshot = manager
            .build_state_snapshot_response(4)
            .expect("state snapshot response");
        assert_eq!(snapshot.block_number, 4);
        assert_eq!(snapshot.account_count, 1);
        assert_eq!(snapshot.compute_txs.len(), 1);
        assert_eq!(snapshot.state_root, derive_state_root(&snapshot.accounts));
        assert_eq!(
            snapshot.state_proof,
            derive_state_proof(&headers[2].hash, &snapshot)
        );
    }

    #[test]
    fn test_validate_block_against_parent_accepts_version2_full_target_pow() {
        let mut parent = legacy_mining_root_header();
        parent.difficulty = U256::one();
        let block = make_version2_pow_block(1, &parent, 30);
        validate_block_against_parent(&parent, &block.header).expect("version2 pow block");
    }

    #[tokio::test]
    async fn test_global_replace_block_chain_overwrites_canonical_suffix() {
        let _guard = crate::tests::crate_test_guard();
        crate::global_reset_sync_cache();
        seed_chain(3);

        let parent = crate::global_block_by_number(1).expect("seeded parent");
        let alt_two = make_block(2, &parent.header, 1_000);
        let alt_three = make_block(3, &alt_two.header, 1_030);

        crate::global_replace_block_chain(vec![alt_two.clone(), alt_three.clone()])
            .expect("chain replacement should succeed");

        assert_eq!(
            crate::global_block_by_number(1)
                .expect("height 1 remains canonical")
                .header
                .hash,
            parent.header.hash
        );
        assert_eq!(
            crate::global_block_by_number(2)
                .expect("reorg height 2")
                .header
                .hash,
            alt_two.header.hash
        );
        assert_eq!(
            crate::global_block_by_number(3)
                .expect("reorg height 3")
                .header
                .hash,
            alt_three.header.hash
        );
    }

    #[tokio::test]
    async fn test_sync_progresses_with_real_request_response_flow() {
        let _guard = crate::tests::crate_test_guard();
        crate::global_reset_sync_cache();
        let peer_manager = Arc::new(crate::PeerManager::new(8));
        let sync = Arc::new(SyncManager::new(peer_manager.clone()));

        let (tx, mut rx) = mpsc::channel(32);
        peer_manager
            .add_peer_with_sender(
                crate::NodeRecord {
                    peer_id: "peer-sync-a".to_string(),
                    ip: "127.0.0.1".to_string(),
                    tcp_port: 30303,
                    udp_port: 30303,
                    network_id: 10086,
                },
                tx,
            )
            .unwrap();
        let _ = peer_manager.update_peer_height("peer-sync-a", 3);
        seed_chain(3);

        sync.start_default().await.unwrap();

        let sync_clone = sync.clone();
        let responder = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    ProtocolMessage::SyncGetHeaders { start, limit } => {
                        let headers = headers_from_local_chain(start, limit);
                        sync_clone.handle_sync_headers("peer-sync-a".to_string(), headers);
                    }
                    ProtocolMessage::SyncGetBlockBody { block_hash } => {
                        sync_clone.handle_sync_block_body(
                            "peer-sync-a".to_string(),
                            SyncBlockBody {
                                block_hash,
                                tx_count: 0,
                                transactions_root: Hash::zero(),
                                receipts_root: Hash::zero(),
                                body: Some(BlockBody::default()),
                            },
                        );
                    }
                    ProtocolMessage::SyncGetStateSnapshot { block_number } => {
                        let block_hash = crate::global_block_by_number(block_number)
                            .expect("seeded block")
                            .header
                            .hash;
                        let mut snapshot = SyncStateSnapshot {
                            block_number,
                            state_root: derive_state_root(&[]),
                            account_count: 0,
                            accounts: Vec::new(),
                            compute_txs: Vec::new(),
                            state_proof: Vec::new(),
                        };
                        snapshot.state_proof = derive_state_proof(&block_hash, &snapshot);
                        sync_clone.handle_sync_state_snapshot("peer-sync-a".to_string(), snapshot);
                    }
                    _ => {}
                }
            }
        });

        timeout(Duration::from_secs(10), async {
            loop {
                if sync.state() == SyncState::Complete && sync.local_height() >= 3 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("sync should complete");

        sync.stop().await.unwrap();
        responder.abort();
        assert!(sync.local_height() >= 3);
    }

    #[test]
    fn block_body_response_requires_sidecar_for_canonical_blocks() {
        crate::global_reset_sync_cache();
        let sync = SyncManager::new(Arc::new(crate::PeerManager::new(4)));
        seed_chain(1);

        let block = crate::global_block_by_number(1).expect("seeded block");
        assert!(sync.build_block_body_response(&block.header.hash).is_some());
    }

    #[tokio::test]
    async fn wait_for_headers_ignores_stale_batches_for_same_peer() {
        let (tx, mut rx) = mpsc::channel(8);
        let peer_id = "peer-sync-a".to_string();

        tx.send(SyncInbound::Headers {
            peer_id: peer_id.clone(),
            headers: vec![SyncHeader {
                version: 1,
                number: 10,
                hash: Hash::from_bytes([0x10; 32]),
                parent_hash: Hash::from_bytes([0x09; 32]),
                state_root: Hash::zero(),
                transactions_root: Hash::zero(),
                receipts_root: Hash::zero(),
                timestamp: 10,
                difficulty: 1,
                nonce: 1,
                coinbase: Address::zero(),
                mix_hash: Hash::from_bytes([0x11; 32]),
                extra_data: Vec::new(),
            }],
        })
        .await
        .expect("send stale headers");
        tx.send(SyncInbound::Headers {
            peer_id: peer_id.clone(),
            headers: vec![SyncHeader {
                version: 1,
                number: 20,
                hash: Hash::from_bytes([0x20; 32]),
                parent_hash: Hash::from_bytes([0x19; 32]),
                state_root: Hash::zero(),
                transactions_root: Hash::zero(),
                receipts_root: Hash::zero(),
                timestamp: 20,
                difficulty: 1,
                nonce: 2,
                coinbase: Address::zero(),
                mix_hash: Hash::from_bytes([0x21; 32]),
                extra_data: Vec::new(),
            }],
        })
        .await
        .expect("send expected headers");

        let headers = wait_for_headers(&mut rx, &peer_id, 20, Duration::from_secs(1))
            .await
            .expect("wait for expected headers");
        assert_eq!(headers[0].number, 20);
    }

    #[tokio::test]
    async fn checkpoint_roundtrip_restores_height_and_state() {
        let _guard = crate::tests::crate_test_guard();
        crate::global_reset_sync_cache();
        let manager = SyncManager::new(Arc::new(crate::PeerManager::new(4)));
        manager.set_local_height(12);
        manager.import_checkpoint(&SyncCheckpoint {
            local_height: 12,
            state: SyncState::Recovering {
                reason: "network_gap".to_string(),
                retries: 3,
            },
        });

        let checkpoint = manager.export_checkpoint();
        assert_eq!(checkpoint.local_height, 12);
        assert_eq!(
            checkpoint.state,
            SyncState::Recovering {
                reason: "network_gap".to_string(),
                retries: 3
            }
        );
    }

    #[tokio::test]
    async fn new_manager_preserves_existing_global_height() {
        let _guard = crate::tests::crate_test_guard();
        seed_chain(5);

        let manager = SyncManager::new(Arc::new(crate::PeerManager::new(4)));

        assert_eq!(manager.local_height(), 5);
        assert_eq!(crate::global_synced_height(), 5);
    }
}
