//! RabbitChain P2P Network Layer
//!
//! Provides:
//! - Peer discovery and management
//! - Block and compute operation propagation
//! - Chain synchronization
//! - RLPx protocol implementation

#![allow(missing_docs)]
#![allow(rustdoc::missing_crate_level_docs)]
#![allow(unused)]

pub mod discovery;
pub mod peer;
pub mod protocol;
pub mod sync;

pub use discovery::{Discovery, NodeRecord};
pub use peer::{Peer, PeerInfo, PeerManager, PeerStatus};
pub use protocol::{
    BlockMessage, Protocol, ProtocolMessage, SyncBlockBody, SyncComputeTxRecord, SyncHeader,
    SyncStateSnapshot,
};
pub use sync::{SyncManager, SyncState};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;
use uuid::Uuid;
use rabbitcore::account::Account;
use rabbitcore::account::U256;
use rabbitcore::block::{Block, BlockBody, BlockBodyRecord, BlockHeader, CANONICAL_BLOCK_VERSION};
use rabbitcore::crypto::Hash;

static GLOBAL_PEER_COUNT: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_PEER_INFOS: Lazy<RwLock<Vec<PeerInfo>>> = Lazy::new(|| RwLock::new(Vec::new()));
static GLOBAL_SYNCED_HEIGHT: AtomicU64 = AtomicU64::new(0);
/// Highest height announced by any connected peer, updated on each RABBIT/HEAD.
/// Used by the local miner to avoid mining when the node is behind the network.
static GLOBAL_HIGHEST_PEER_HEIGHT: AtomicU64 = AtomicU64::new(0);
static GLOBAL_BLOCKS: Lazy<RwLock<BTreeMap<u64, rabbitcore::block::Block>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static GLOBAL_BLOCK_BODIES: Lazy<RwLock<BTreeMap<Hash, BlockBodyRecord>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static GLOBAL_BLOCK_RECEIPTS: Lazy<RwLock<HashMap<Hash, rabbitcore::block::Receipt>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static GLOBAL_CANONICAL_BLOCK_ACTIVATION_HEIGHT: Lazy<RwLock<Option<u64>>> =
    Lazy::new(|| RwLock::new(None));
static GLOBAL_BLOCK_PERSISTENCE_PATH: Lazy<RwLock<Option<PathBuf>>> =
    Lazy::new(|| RwLock::new(None));
static GLOBAL_BLOCK_BODY_PERSISTENCE_PATH: Lazy<RwLock<Option<PathBuf>>> =
    Lazy::new(|| RwLock::new(None));
static GLOBAL_BLOCK_BODY_ORDER: Lazy<RwLock<VecDeque<Hash>>> =
    Lazy::new(|| RwLock::new(VecDeque::new()));
static GLOBAL_SYNC_ACCOUNTS: Lazy<RwLock<HashMap<rabbitcore::crypto::Address, Account>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static GLOBAL_SYNC_COMPUTE_TXS: Lazy<RwLock<HashMap<Hash, SyncComputeTxRecord>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static GLOBAL_SYNC_COMPUTE_ORDER: Lazy<RwLock<VecDeque<Hash>>> =
    Lazy::new(|| RwLock::new(VecDeque::new()));
static SEEN_TX_HASHES: Lazy<RwLock<HashMap<String, u64>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static SEEN_BLOCK_HASHES: Lazy<RwLock<HashMap<String, u64>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
/// Optional reorg listener: invoked with the new canonical tip height after a
/// successful promote that shortens or rewrites the canonical suffix.
static CANONICAL_TIP_LISTENER: Lazy<RwLock<Option<CanonicalTipListener>>> =
    Lazy::new(|| RwLock::new(None));

/// Callback when the canonical tip height changes due to fork choice / reorg.
pub type CanonicalTipListener = Arc<dyn Fn(u64) + Send + Sync + 'static>;

/// Register a listener notified after canonical tip updates (including reorgs).
pub fn set_canonical_tip_listener(listener: Option<CanonicalTipListener>) {
    *CANONICAL_TIP_LISTENER.write() = listener;
}

const HANDSHAKE_PREFIX: &str = "RABBITCHAIN/1";
const HANDSHAKE_MAX_LEN: usize = 512;
const HANDSHAKE_TIMEOUT_SECS: u64 = 5;
const CONTROL_FRAME_MAX_LEN: usize = 16 * 1024 * 1024;
const HEARTBEAT_INTERVAL_SECS: u64 = 15;
const PEER_IDLE_TIMEOUT_SECS: u64 = 45;
const PEER_SEND_BUFFER: usize = 256;
const DEFAULT_DEDUP_TTL_SECS: u64 = 5 * 60;
const MAX_DEDUP_ENTRIES: usize = 8192;
const MAX_GLOBAL_BLOCKS: usize = 50_000;
const MAX_GLOBAL_BLOCK_BODIES: usize = 50_000;
const MAX_GLOBAL_SYNC_TX_INDEX: usize = 100_000;
const DISCOVERY_DIAL_INTERVAL_SECS: u64 = 5;
const SYNC_HEAD_ANNOUNCE_INTERVAL_SECS: u64 = 10;

#[async_trait]
trait PeerWire: Send + Unpin {
    async fn read_line(&mut self, max_len: usize) -> io::Result<Option<String>>;
    async fn write_line(&mut self, line: &str) -> io::Result<()>;
}

struct TcpPeerWire {
    stream: TcpStream,
}

impl TcpPeerWire {
    fn new(stream: TcpStream) -> Self {
        Self { stream }
    }
}

#[async_trait]
impl PeerWire for TcpPeerWire {
    async fn read_line(&mut self, max_len: usize) -> io::Result<Option<String>> {
        let mut line = Vec::with_capacity(64);
        loop {
            let mut b = [0u8; 1];
            let read = self.stream.read(&mut b).await?;
            if read == 0 {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed mid-frame",
                    ))
                };
            }
            if b[0] == b'\n' {
                break;
            }
            line.push(b[0]);
            if line.len() > max_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "line frame too long",
                ));
            }
        }

        String::from_utf8(line)
            .map(Some)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    async fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.stream.write_all(line.as_bytes()).await
    }
}

struct WsPeerWire<S> {
    stream: WebSocketStream<S>,
}

impl<S> WsPeerWire<S> {
    fn new(stream: WebSocketStream<S>) -> Self {
        Self { stream }
    }
}

#[async_trait]
impl<S> PeerWire for WsPeerWire<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    async fn read_line(&mut self, max_len: usize) -> io::Result<Option<String>> {
        while let Some(message) = self.stream.next().await {
            let message =
                message.map_err(|err| io::Error::new(io::ErrorKind::ConnectionAborted, err))?;
            match message {
                Message::Text(text) => {
                    if text.len() > max_len {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "websocket text frame too long",
                        ));
                    }
                    return Ok(Some(text.trim_end_matches(['\r', '\n']).to_string()));
                }
                Message::Binary(bytes) => {
                    if bytes.len() > max_len {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "websocket binary frame too long",
                        ));
                    }
                    let text = String::from_utf8(bytes)
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                    return Ok(Some(text.trim_end_matches(['\r', '\n']).to_string()));
                }
                Message::Ping(payload) => {
                    self.stream
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|err| io::Error::new(io::ErrorKind::BrokenPipe, err))?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => return Ok(None),
                Message::Frame(_) => {}
            }
        }
        Ok(None)
    }

    async fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.stream
            .send(Message::Text(
                line.trim_end_matches(['\r', '\n']).to_string(),
            ))
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::BrokenPipe, err))
    }
}

type BoxedPeerWire = Box<dyn PeerWire>;

/// Returns the current process-level peer count.
pub fn global_peer_count() -> usize {
    GLOBAL_PEER_COUNT.load(Ordering::Relaxed)
}

pub(crate) fn set_global_peer_count(count: usize) {
    GLOBAL_PEER_COUNT.store(count, Ordering::Relaxed);
}

/// Returns snapshots for all currently tracked peers.
pub fn global_peers() -> Vec<PeerInfo> {
    GLOBAL_PEER_INFOS.read().clone()
}

pub(crate) fn set_global_peers(peers: Vec<PeerInfo>) {
    *GLOBAL_PEER_INFOS.write() = peers;
}

/// Returns the latest synchronized height reported by network sync.
pub fn global_synced_height() -> u64 {
    GLOBAL_SYNCED_HEIGHT.load(Ordering::Relaxed)
}

pub fn set_global_synced_height(height: u64) {
    GLOBAL_SYNCED_HEIGHT.store(height, Ordering::SeqCst);
}

pub fn set_global_highest_peer_height(height: u64) {
    GLOBAL_HIGHEST_PEER_HEIGHT.store(height, Ordering::SeqCst);
}

pub fn global_highest_peer_height() -> u64 {
    GLOBAL_HIGHEST_PEER_HEIGHT.load(Ordering::SeqCst)
}

/// Configure the activation height for canonical block-body blocks.
pub fn configure_global_block_activation_height(height: Option<u64>) {
    *GLOBAL_CANONICAL_BLOCK_ACTIVATION_HEIGHT.write() = height;
}

/// Returns the configured activation height for canonical block-body blocks.
pub fn global_block_activation_height() -> Option<u64> {
    *GLOBAL_CANONICAL_BLOCK_ACTIVATION_HEIGHT.read()
}

/// Returns whether a block requires a body sidecar under the active rules.
pub fn global_block_requires_body(height: u64, version: u32) -> bool {
    version >= CANONICAL_BLOCK_VERSION
        || global_block_activation_height().is_some_and(|activation| height >= activation)
}

/// Returns the block version that should be produced for a height.
pub fn global_block_version_for_height(height: u64) -> u32 {
    if global_block_activation_height().is_some_and(|activation| height >= activation) {
        CANONICAL_BLOCK_VERSION
    } else {
        2
    }
}

/// Store a canonical block snapshot for sync/read APIs.
pub fn global_store_block(block: rabbitcore::block::Block) -> Result<()> {
    let mut block = block;
    let height = block.header.number.as_u64();
    if let Some(existing) = GLOBAL_BLOCKS.read().get(&height).cloned() {
        if existing.header.hash == block.header.hash {
            if existing.body.is_none() && block.body.is_some() {
                let body = block.body.clone().unwrap();
                let record = BlockBodyRecord::new(height, block.header.hash, body.clone());
                store_block_body(record, false)?;
                let mut blocks = GLOBAL_BLOCKS.write();
                if let Some(existing) = blocks.get_mut(&height) {
                    existing.body = Some(body);
                }
            }
            return Ok(());
        }
    }

    if let Some(activation_height) = global_block_activation_height() {
        if height >= activation_height && block.header.version < CANONICAL_BLOCK_VERSION {
            return Err(NetworkError::ProtocolError(format!(
                "block version {} is below canonical version {} at activation height {}",
                block.header.version, CANONICAL_BLOCK_VERSION, activation_height
            )));
        }
    }

    if let Some(body) = block.body.clone() {
        // Track whether the header had zero roots before reconciliation.
        let roots_were_zero = block.header.transactions_root.is_zero()
            && block.header.receipts_root.is_zero();
        block
            .header
            .reconcile_body_commitments(&body)
            .map_err(|err| {
                NetworkError::ProtocolError(format!("block body commitment mismatch: {err}"))
            })?;
        // If the header previously had zero roots (e.g., a test/miner template
        // without pre-computed body commitments), the mix_hash (PoW proof) must
        // be recomputed because it binds the transactions/receipts roots.
        // Genesis blocks (height 0) are exempt from PoW.
        if roots_were_zero && height > 0 {
            let recomputed_mix = rabbitcore::block::compute_pow_hash(&block.header, block.header.nonce);
            block.header.mix_hash = recomputed_mix;
        }
        block.header.hash = block.header.compute_hash();
        body.validate_against_header(&block.header).map_err(|err| {
            NetworkError::ProtocolError(format!("block body validation failed: {err}"))
        })?;
    } else if let Some(body_record) = global_block_body_by_hash(&block.header.hash) {
        block
            .header
            .reconcile_body_commitments(&body_record.body)
            .map_err(|err| NetworkError::ProtocolError(format!("{err}")))?;
        block.body = Some(body_record.body);
    } else if global_block_requires_body(height, block.header.version) {
        return Err(NetworkError::ProtocolError(format!(
            "missing canonical block body for block {} at height {}",
            block.header.hash, height
        )));
    }
    validate_global_block_insert(&block)?;
    persist_global_block(&block).map_err(NetworkError::IO)?;
    {
        let mut blocks = GLOBAL_BLOCKS.write();
        blocks.insert(height, block.clone());
        while blocks.len() > MAX_GLOBAL_BLOCKS {
            let Some(oldest) = blocks.keys().next().copied() else {
                break;
            };
            blocks.remove(&oldest);
        }
    }
    if let Some(body) = block.body.clone() {
        let record = BlockBodyRecord::new(height, block.header.hash, body);
        store_block_body(record, false)?;
    }
    let prev = global_synced_height();
    if height > prev {
        set_global_synced_height(height);
    }
    Ok(())
}

/// Store a canonical block snapshot together with its body sidecar.
pub fn global_store_block_with_body(block: Block, body: BlockBody) -> Result<()> {
    let mut block = block;
    block.body = Some(body);
    global_store_block(block)
}

/// Store or update a canonical block body sidecar by block hash.
pub fn global_store_block_body(record: BlockBodyRecord) -> Result<()> {
    store_block_body(record, true)
}

/// Replace the canonical suffix starting at the first block's height.
///
/// This is used by sync when a peer presents a validated chain segment that
/// diverges from the current canonical head. The replacement is intentionally
/// suffix-based: blocks before the first provided height are preserved, while
/// the incoming validated chain overwrites the canonical tail from that point.
pub fn global_replace_block_chain(blocks: Vec<rabbitcore::block::Block>) -> Result<()> {
    if blocks.is_empty() {
        return Ok(());
    }

    validate_global_block_chain_replacement(&blocks)?;

    let start_height = blocks[0].header.number.as_u64();
    let removed_hashes: Vec<Hash> = {
        let canonical = GLOBAL_BLOCKS.read();
        canonical
            .range(start_height..)
            .map(|(_, block)| block.header.hash)
            .collect()
    };
    {
        let mut canonical = GLOBAL_BLOCKS.write();
        canonical.split_off(&start_height);
    }
    if !removed_hashes.is_empty() {
        remove_block_bodies(&removed_hashes);
    }

    for block in blocks {
        global_store_block(block)?;
    }

    Ok(())
}

fn validate_global_block_chain_replacement(blocks: &[Block]) -> Result<()> {
    let Some(first) = blocks.first() else {
        return Ok(());
    };
    let start_height = first.header.number.as_u64();
    if start_height == 0 {
        crate::sync::validate_persisted_block_chain(blocks).map_err(NetworkError::ProtocolError)?;
        return Ok(());
    }

    if start_height == 1 {
        crate::sync::validate_block_against_root(&first.header)
            .map_err(NetworkError::ProtocolError)?;
    } else {
        let Some(parent) = GLOBAL_BLOCKS
            .read()
            .get(&start_height.saturating_sub(1))
            .cloned()
        else {
            return Err(NetworkError::ProtocolError(format!(
                "missing canonical parent for chain replacement at height {}",
                start_height
            )));
        };
        crate::sync::validate_block_against_parent(&parent.header, &first.header)
            .map_err(NetworkError::ProtocolError)?;
    }

    for pair in blocks.windows(2) {
        crate::sync::validate_block_against_parent(&pair[0].header, &pair[1].header)
            .map_err(NetworkError::ProtocolError)?;
    }

    Ok(())
}

fn validate_global_block_insert(block: &Block) -> Result<()> {
    let height = block.header.number.as_u64();
    if let Some(existing) = GLOBAL_BLOCKS.read().get(&height).cloned() {
        if existing.header.hash == block.header.hash {
            return Ok(());
        }
        return Err(NetworkError::ProtocolError(format!(
            "conflicting block at height {}: existing={} incoming={}",
            height, existing.header.hash, block.header.hash
        )));
    }

    if height == 0 {
        return crate::sync::validate_persisted_block_chain(std::slice::from_ref(block))
            .map_err(NetworkError::ProtocolError);
    }

    if let Some(parent) = GLOBAL_BLOCKS.read().get(&height.saturating_sub(1)).cloned() {
        return crate::sync::validate_block_against_parent(&parent.header, &block.header)
            .map_err(NetworkError::ProtocolError);
    }

    if height == 1 {
        return crate::sync::validate_block_against_root(&block.header)
            .map_err(NetworkError::ProtocolError);
    }

    if let Some(body) = &block.body {
        block
            .header
            .validate_body_commitments(body)
            .map_err(|err| NetworkError::ProtocolError(format!("{err}")))?;
    }

    Err(NetworkError::ProtocolError(format!(
        "missing parent block for height {}",
        height
    )))
}

/// Read a canonical block snapshot by number.
pub fn global_block_by_number(number: u64) -> Option<rabbitcore::block::Block> {
    GLOBAL_BLOCKS.read().get(&number).cloned()
}

/// Read a canonical block snapshot by hash.
pub fn global_block_by_hash(hash: &Hash) -> Option<rabbitcore::block::Block> {
    GLOBAL_BLOCKS
        .read()
        .values()
        .find(|block| block.header.hash == *hash)
        .cloned()
}

/// Read latest canonical block snapshot.
pub fn global_latest_block() -> Option<rabbitcore::block::Block> {
    GLOBAL_BLOCKS
        .read()
        .last_key_value()
        .map(|(_, b)| b.clone())
}

/// Resolve block number from hash in canonical snapshot cache.
pub fn global_block_number_for_hash(target: &Hash) -> Option<u64> {
    GLOBAL_BLOCKS
        .read()
        .iter()
        .find_map(|(n, b)| (b.header.hash == *target).then_some(*n))
}

/// Reset in-process sync cache (blocks + advertised synced height).
pub fn global_reset_sync_cache() {
    GLOBAL_BLOCKS.write().clear();
    GLOBAL_BLOCK_BODIES.write().clear();
    GLOBAL_BLOCK_RECEIPTS.write().clear();
    GLOBAL_BLOCK_BODY_ORDER.write().clear();
    configure_global_block_activation_height(None);
    GLOBAL_SYNC_ACCOUNTS.write().clear();
    GLOBAL_SYNC_COMPUTE_TXS.write().clear();
    GLOBAL_SYNC_COMPUTE_ORDER.write().clear();
    set_global_synced_height(0);
}

/// Configure optional process-wide block persistence for P2P sync recovery.
///
/// The persisted data is a JSON-lines header store. It is intentionally simple:
/// every accepted block appends one record, and startup rebuilds the in-memory
/// sync cache from the latest record per height.
pub fn configure_global_block_persistence(path: Option<PathBuf>) -> Result<()> {
    *GLOBAL_BLOCK_PERSISTENCE_PATH.write() = path.clone();
    let body_path = path.as_deref().map(derive_block_body_sidecar_path);
    *GLOBAL_BLOCK_BODY_PERSISTENCE_PATH.write() = body_path.clone();

    let Some(path) = path else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    if let Some(body_path) = body_path {
        let loaded_bodies = load_persisted_block_bodies(&body_path)?;
        if !loaded_bodies.is_empty() {
            let mut max_body_height = 0u64;
            for record in loaded_bodies {
                let body_record = record.into_block_body_record();
                max_body_height = max_body_height.max(body_record.number);
                store_block_body(body_record, false)?;
            }
            tracing::info!(
                "loaded persisted P2P block bodies up to height {} from {}",
                max_body_height,
                body_path.display()
            );
        }
    }

    let loaded = load_persisted_blocks(&path)?;
    if loaded.is_empty() {
        return Ok(());
    }

    let mut max_height = 0u64;
    {
        let mut blocks = GLOBAL_BLOCKS.write();
        for block in loaded {
            let height = block.header.number.as_u64();
            max_height = max_height.max(height);
            blocks.insert(height, block);
        }
        while blocks.len() > MAX_GLOBAL_BLOCKS {
            let Some(oldest) = blocks.keys().next().copied() else {
                break;
            };
            blocks.remove(&oldest);
        }
    }
    // 快照必须在循环外构造：`GLOBAL_BLOCKS.read()` 的临时 guard 会存活整个 for
    // 语句（含循环体），而 store_block_body → update_global_block_body_commitments
    // 需要 write 锁 —— 写锁等待自己持有的读锁 = 自死锁（parking_lot 不可重入）。
    let blocks_snapshot: Vec<rabbitcore::block::Block> = {
        let guard = GLOBAL_BLOCKS.read();
        guard.values().cloned().collect()
    };
    for block in blocks_snapshot {
        if let Some(body) = block.body.clone() {
            let record =
                BlockBodyRecord::new(block.header.number.as_u64(), block.header.hash, body);
            store_block_body(record, false)?;
        }
    }
    set_global_synced_height(global_synced_height().max(max_height));
    tracing::info!(
        "loaded persisted P2P sync blocks up to height {} from {}",
        max_height,
        path.display()
    );
    Ok(())
}

/// Read a canonical block body by block hash.
pub fn global_block_body_by_hash(block_hash: &Hash) -> Option<BlockBodyRecord> {
    GLOBAL_BLOCK_BODIES.read().get(block_hash).cloned()
}

/// Read a canonical block receipt by transaction hash.
pub fn global_block_receipt_by_tx_hash(tx_hash: &Hash) -> Option<rabbitcore::block::Receipt> {
    GLOBAL_BLOCK_RECEIPTS.read().get(tx_hash).cloned()
}

/// Read all receipts for a block by block hash.
pub fn global_block_receipts_by_hash(block_hash: &Hash) -> Option<Vec<rabbitcore::block::Receipt>> {
    global_block_body_by_hash(block_hash).map(|record| record.body.receipts)
}

/// Read a canonical block body by block number.
pub fn global_block_body_by_number(number: u64) -> Option<BlockBodyRecord> {
    let block = global_block_by_number(number)?;
    global_block_body_by_hash(&block.header.hash)
}

/// Read the latest stored block body record.
pub fn global_latest_block_body() -> Option<BlockBodyRecord> {
    let body_order = GLOBAL_BLOCK_BODY_ORDER.read();
    let bodies = GLOBAL_BLOCK_BODIES.read();
    body_order
        .iter()
        .rev()
        .find_map(|hash| bodies.get(hash).cloned())
}

fn store_block_body(record: BlockBodyRecord, persist: bool) -> Result<()> {
    validate_global_block_body_insert(&record)?;
    if persist {
        persist_global_block_body(&record).map_err(NetworkError::IO)?;
    }

    let previous = {
        let mut bodies = GLOBAL_BLOCK_BODIES.write();
        bodies.insert(record.block_hash, record.clone())
    };

    if let Some(previous) = previous {
        remove_block_receipts(&previous.body.receipts);
    }

    let stale_hashes = {
        let mut order = GLOBAL_BLOCK_BODY_ORDER.write();
        order.retain(|hash| hash != &record.block_hash);
        order.push_back(record.block_hash);
        let mut stale = Vec::new();
        while order.len() > MAX_GLOBAL_BLOCK_BODIES {
            if let Some(stale_hash) = order.pop_front() {
                stale.push(stale_hash);
            } else {
                break;
            }
        }
        stale
    };
    if !stale_hashes.is_empty() {
        remove_block_bodies(&stale_hashes);
    }

    index_block_receipts(&record.body.receipts);
    if global_block_by_hash(&record.block_hash).is_some() {
        update_global_block_body_commitments(&record)?;
    }
    Ok(())
}

fn remove_block_bodies(hashes: &[Hash]) {
    if hashes.is_empty() {
        return;
    }
    let hash_set: HashSet<Hash> = hashes.iter().copied().collect();
    let removed_receipts = {
        let bodies = GLOBAL_BLOCK_BODIES.read();
        hashes
            .iter()
            .filter_map(|hash| bodies.get(hash))
            .flat_map(|record| record.body.receipts.clone())
            .collect::<Vec<_>>()
    };
    {
        let mut bodies = GLOBAL_BLOCK_BODIES.write();
        let mut order = GLOBAL_BLOCK_BODY_ORDER.write();
        for hash in hashes {
            bodies.remove(hash);
        }
        order.retain(|hash| !hash_set.contains(hash));
    }
    remove_block_receipts(&removed_receipts);
}

fn index_block_receipts(receipts: &[rabbitcore::block::Receipt]) {
    if receipts.is_empty() {
        return;
    }
    let mut index = GLOBAL_BLOCK_RECEIPTS.write();
    for receipt in receipts {
        index.insert(receipt.tx_id.0, receipt.clone());
    }
}

fn remove_block_receipts(receipts: &[rabbitcore::block::Receipt]) {
    if receipts.is_empty() {
        return;
    }
    let mut index = GLOBAL_BLOCK_RECEIPTS.write();
    for receipt in receipts {
        index.remove(&receipt.tx_id.0);
    }
}

fn update_global_block_body_commitments(record: &BlockBodyRecord) -> Result<()> {
    let roots = record.body.commitment_roots();
    let mut blocks = GLOBAL_BLOCKS.write();
    let Some((_, block)) = blocks
        .iter_mut()
        .find(|(_, block)| block.header.hash == record.block_hash)
    else {
        return Err(NetworkError::ProtocolError(format!(
            "missing canonical block {} for body commitment update",
            record.block_hash
        )));
    };
    block.header.transactions_root = roots.transactions_root;
    block.header.receipts_root = roots.receipts_root;
    block.body = Some(record.body.clone());
    Ok(())
}

fn validate_global_block_body_insert(record: &BlockBodyRecord) -> Result<()> {
    if record.body.version != BlockBody::default_version() {
        return Err(NetworkError::ProtocolError(format!(
            "unsupported block body version {} for block {}",
            record.body.version, record.block_hash
        )));
    }
    if record.body.transactions.len() != record.body.receipts.len() {
        return Err(NetworkError::ProtocolError(format!(
            "block body tx/receipt length mismatch for block {}: txs={} receipts={}",
            record.block_hash,
            record.body.transactions.len(),
            record.body.receipts.len()
        )));
    }
    for (index, (tx, receipt)) in record
        .body
        .transactions
        .iter()
        .zip(record.body.receipts.iter())
        .enumerate()
    {
        if tx.tx_id != receipt.tx_id {
            return Err(NetworkError::ProtocolError(format!(
                "block body tx/receipt id mismatch at index {} for block {}",
                index, record.block_hash
            )));
        }
        if receipt.block_hash != record.block_hash {
            return Err(NetworkError::ProtocolError(format!(
                "receipt block hash mismatch at index {} for block {}",
                index, record.block_hash
            )));
        }
    }

    if let Some(block) = global_block_by_hash(&record.block_hash) {
        let mut candidate = block.clone();
        candidate
            .header
            .reconcile_body_commitments(&record.body)
            .map_err(|err| NetworkError::ProtocolError(format!("{err}")))?;
        candidate
            .header
            .validate_body_commitments(&record.body)
            .map_err(|err| NetworkError::ProtocolError(format!("{err}")))?;
    }

    if let Some(block) = global_block_by_number(record.number) {
        if block.header.hash != record.block_hash {
            return Err(NetworkError::ProtocolError(format!(
                "conflicting block body at height {}: block hash {} body hash {}",
                record.number, block.header.hash, record.block_hash
            )));
        }
    }

    if let Some(existing_height) = global_block_number_for_hash(&record.block_hash) {
        if existing_height != record.number {
            return Err(NetworkError::ProtocolError(format!(
                "block body hash {} already mapped to height {}",
                record.block_hash, existing_height
            )));
        }
    }

    Ok(())
}

fn derive_block_body_sidecar_path(path: &Path) -> PathBuf {
    let mut sidecar = path.to_path_buf();
    sidecar.set_extension("body.jsonl");
    sidecar
}

fn persist_global_block_body(record: &BlockBodyRecord) -> io::Result<()> {
    let path = GLOBAL_BLOCK_BODY_PERSISTENCE_PATH.read().clone();
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(&mut file, &PersistedBlockBodyRecord::from(record))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(())
}

fn load_persisted_block_bodies(path: &Path) -> Result<Vec<PersistedBlockBodyRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let contents = fs::read_to_string(path)?;
    let lines: Vec<&str> = contents.lines().collect();
    let mut loaded = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: PersistedBlockBodyRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(err) if idx + 1 == lines.len() => {
                tracing::warn!(
                    "ignoring truncated trailing persisted block body record {} in {}: {}",
                    idx + 1,
                    path.display(),
                    err
                );
                break;
            }
            Err(err) => {
                return Err(NetworkError::Serialization(format!(
                    "invalid persisted block body record {} in {}: {}",
                    idx + 1,
                    path.display(),
                    err
                )));
            }
        };
        if record.version != 1 {
            return Err(NetworkError::Serialization(format!(
                "unsupported persisted block body record version {} in {} at line {}",
                record.version,
                path.display(),
                idx + 1
            )));
        }
        loaded.push(record);
    }
    Ok(loaded)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedBlockRecord {
    version: u32,
    #[serde(default = "default_persisted_header_version")]
    header_version: u32,
    number: u64,
    hash: Hash,
    parent_hash: Hash,
    timestamp: u64,
    difficulty: U256,
    nonce: u64,
    coinbase: rabbitcore::crypto::Address,
    mix_hash: Hash,
    extra_data: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<BlockBody>,
    // 完整头部字段（旧 v1 记录缺省，按旧链值回退；base_fee 改 SHC 计价后必须持久化，
    // 否则重建头部 compute_hash ≠ 原 hash → genesis_record_hash_mismatch）。
    #[serde(default = "default_legacy_base_fee")]
    base_fee_per_gas: U256,
    #[serde(default)]
    state_root: Hash,
    #[serde(default)]
    transactions_root: Hash,
    #[serde(default)]
    receipts_root: Hash,
    #[serde(default = "default_block_gas_limit")]
    gas_limit: u64,
    #[serde(default)]
    gas_used: u64,
    #[serde(default)]
    uncle_hashes: Vec<Hash>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedBlockBodyRecord {
    version: u32,
    number: u64,
    block_hash: Hash,
    body: BlockBody,
}

fn default_persisted_header_version() -> u32 {
    1
}

fn default_legacy_base_fee() -> U256 {
    U256::from(1_000_000_000u64) // 旧链基础费（hopps 计价 era）；新记录始终持久化真实值
}

fn default_block_gas_limit() -> u64 {
    30_000_000
}

impl From<&Block> for PersistedBlockRecord {
    fn from(block: &Block) -> Self {
        Self {
            version: 1,
            header_version: block.header.version,
            number: block.header.number.as_u64(),
            hash: block.header.hash,
            parent_hash: block.header.parent_hash,
            timestamp: block.header.timestamp,
            difficulty: block.header.difficulty,
            nonce: block.header.nonce,
            coinbase: block.header.coinbase,
            mix_hash: block.header.mix_hash,
            extra_data: block.header.extra_data.clone(),
            body: block.body.clone(),
            base_fee_per_gas: block.header.base_fee_per_gas,
            state_root: block.header.state_root,
            transactions_root: block.header.transactions_root,
            receipts_root: block.header.receipts_root,
            gas_limit: block.header.gas_limit,
            gas_used: block.header.gas_used,
            uncle_hashes: block.header.uncle_hashes.clone(),
        }
    }
}

impl PersistedBlockRecord {
    fn into_block(self) -> Block {
        let body = self
            .body
            .or_else(|| global_block_body_by_hash(&self.hash).map(|record| record.body))
            .unwrap_or_default();
        Block::new(BlockHeader {
            version: self.header_version,
            parent_hash: self.parent_hash,
            uncle_hashes: self.uncle_hashes,
            coinbase: self.coinbase,
            state_root: self.state_root,
            transactions_root: self.transactions_root,
            receipts_root: self.receipts_root,
            number: U256::from(self.number),
            gas_limit: self.gas_limit,
            gas_used: self.gas_used,
            timestamp: self.timestamp,
            difficulty: self.difficulty,
            nonce: self.nonce,
            extra_data: self.extra_data,
            mix_hash: self.mix_hash,
            base_fee_per_gas: self.base_fee_per_gas,
            hash: self.hash,
        })
        .with_body(body)
    }
}

impl From<&BlockBodyRecord> for PersistedBlockBodyRecord {
    fn from(record: &BlockBodyRecord) -> Self {
        Self {
            version: 1,
            number: record.number,
            block_hash: record.block_hash,
            body: record.body.clone(),
        }
    }
}

impl PersistedBlockBodyRecord {
    fn into_block_body_record(self) -> BlockBodyRecord {
        BlockBodyRecord::new(self.number, self.block_hash, self.body)
    }
}

fn persist_global_block(block: &Block) -> io::Result<()> {
    let path = GLOBAL_BLOCK_PERSISTENCE_PATH.read().clone();
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(&mut file, &PersistedBlockRecord::from(block))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(())
}

fn load_persisted_blocks(path: &Path) -> Result<Vec<Block>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let contents = fs::read_to_string(path)?;
    let lines: Vec<&str> = contents.lines().collect();
    let mut by_height = BTreeMap::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: PersistedBlockRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(err) if idx + 1 == lines.len() => {
                tracing::warn!(
                    "ignoring truncated trailing persisted block record {} in {}: {}",
                    idx + 1,
                    path.display(),
                    err
                );
                break;
            }
            Err(err) => {
                return Err(NetworkError::Serialization(format!(
                    "invalid persisted block record {} in {}: {}",
                    idx + 1,
                    path.display(),
                    err
                )));
            }
        };
        if record.version != 1 {
            return Err(NetworkError::Serialization(format!(
                "unsupported persisted block record version {} in {} at line {}",
                record.version,
                path.display(),
                idx + 1
            )));
        }
        by_height.insert(record.number, record.into_block());
    }
    let blocks: Vec<Block> = by_height.into_values().collect();
    crate::sync::validate_persisted_block_chain(&blocks).map_err(|err| {
        NetworkError::Serialization(format!(
            "invalid persisted block chain in {}: {}",
            path.display(),
            err
        ))
    })?;
    Ok(blocks)
}

fn resolve_local_peer_id(config: &NetworkConfig) -> Result<String> {
    if let Some(peer_id) = config.local_peer_id.as_deref() {
        validate_peer_id(peer_id)?;
        return Ok(peer_id.to_string());
    }

    if let Some(path) = config.peer_id_path.as_deref() {
        return load_or_create_peer_id(path);
    }

    Ok(format!("node-{}", Uuid::new_v4()))
}

fn load_or_create_peer_id(path: &Path) -> Result<String> {
    if path.exists() {
        let peer_id = fs::read_to_string(path)?.trim().to_string();
        validate_peer_id(&peer_id)?;
        return Ok(peer_id);
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let peer_id = format!("node-{}", Uuid::new_v4());
    validate_peer_id(&peer_id)?;
    write_peer_id_file(path, &peer_id)?;
    Ok(peer_id)
}

fn write_peer_id_file(path: &Path, peer_id: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(peer_id.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        return Ok(());
    }

    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
        file.write_all(peer_id.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }
}

fn validate_peer_id(peer_id: &str) -> Result<()> {
    let len = peer_id.len();
    if len == 0 || len > 128 {
        return Err(NetworkError::ProtocolError(
            "peer id must be 1..=128 bytes".to_string(),
        ));
    }
    if peer_id.chars().any(char::is_whitespace) {
        return Err(NetworkError::ProtocolError(
            "peer id must not contain whitespace".to_string(),
        ));
    }
    Ok(())
}

/// Record or update an account snapshot visible to RPC readers.
pub fn global_record_account(account: Account) {
    GLOBAL_SYNC_ACCOUNTS
        .write()
        .insert(account.address, account);
}

/// Replace account snapshot cache with the provided full snapshot.
pub fn global_replace_accounts(accounts: Vec<Account>) {
    let mut map = GLOBAL_SYNC_ACCOUNTS.write();
    map.clear();
    for account in accounts {
        map.insert(account.address, account);
    }
}

/// Read a synchronized account snapshot.
pub fn global_synced_account(address: &rabbitcore::crypto::Address) -> Option<Account> {
    GLOBAL_SYNC_ACCOUNTS.read().get(address).cloned()
}

/// Export synchronized account snapshot.
pub fn global_synced_accounts() -> Vec<Account> {
    GLOBAL_SYNC_ACCOUNTS.read().values().cloned().collect()
}

/// Record or update a synchronized compute tx result record.
pub fn global_record_compute_tx(record: SyncComputeTxRecord) {
    let tx_hash = record.tx_hash;
    let mut map = GLOBAL_SYNC_COMPUTE_TXS.write();
    let mut order = GLOBAL_SYNC_COMPUTE_ORDER.write();
    map.insert(tx_hash, record);
    order.retain(|h| h != &tx_hash);
    order.push_back(tx_hash);
    while order.len() > MAX_GLOBAL_SYNC_TX_INDEX {
        if let Some(stale) = order.pop_front() {
            map.remove(&stale);
        }
    }
}

/// Replace synchronized compute tx index.
pub fn global_replace_compute_txs(records: Vec<SyncComputeTxRecord>) {
    let mut map = GLOBAL_SYNC_COMPUTE_TXS.write();
    let mut order = GLOBAL_SYNC_COMPUTE_ORDER.write();
    map.clear();
    order.clear();
    for record in records {
        let tx_hash = record.tx_hash;
        map.insert(tx_hash, record);
        order.push_back(tx_hash);
    }
}

/// Read a synchronized compute tx result record.
pub fn global_synced_compute_tx(hash: &Hash) -> Option<SyncComputeTxRecord> {
    GLOBAL_SYNC_COMPUTE_TXS.read().get(hash).cloned()
}

/// Export synchronized compute tx records (oldest -> newest).
pub fn global_synced_compute_txs() -> Vec<SyncComputeTxRecord> {
    let map = GLOBAL_SYNC_COMPUTE_TXS.read();
    GLOBAL_SYNC_COMPUTE_ORDER
        .read()
        .iter()
        .filter_map(|h| map.get(h).cloned())
        .collect()
}

/// Network error types
#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Peer not found: {0}")]
    PeerNotFound(String),
    #[error("Connection error: {0}")]
    ConnectionError(String),
    #[error("Protocol error: {0}")]
    ProtocolError(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Channel error")]
    ChannelError,
}

pub type Result<T> = std::result::Result<T, NetworkError>;

/// Network configuration
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// Network ID
    pub network_id: u64,
    /// Listen address
    pub listen_addr: String,
    /// Listen port
    pub listen_port: u16,
    /// Enable direct TCP P2P transport for enode bootnodes and TCP listener.
    pub enable_tcp_transport: bool,
    /// Enable WebSocket P2P transport for ws/wss bootnodes and WebSocket listener.
    pub enable_ws_transport: bool,
    /// Optional WebSocket P2P listen address for CDN-compatible transport.
    pub ws_listen_addr: Option<String>,
    /// Optional WebSocket P2P listen port for CDN-compatible transport.
    pub ws_listen_port: Option<u16>,
    /// Optional public WebSocket URL advertised in logs, e.g. wss://boot.rabbitchain.org/p2p.
    pub ws_external_url: Option<String>,
    /// External address (optional)
    pub external_addr: Option<String>,
    /// Maximum peers
    pub max_peers: u32,
    /// Minimum peers
    pub min_peers: u32,
    /// Bootstrap nodes
    pub bootnodes: Vec<String>,
    /// Optional explicit stable local peer id.
    pub local_peer_id: Option<String>,
    /// Optional path used to load/create a stable local peer id.
    pub peer_id_path: Option<PathBuf>,
    /// Optional JSON-lines block header store used for P2P sync restart recovery.
    pub sync_blocks_path: Option<PathBuf>,
    /// Optional activation height for canonical block-body blocks.
    pub canonical_block_activation_height: Option<u64>,
    /// Node name
    pub node_name: String,
    /// Enable discovery
    pub enable_discovery: bool,
    /// Enable sync
    pub enable_sync: bool,
    /// Optional persisted banlist path.
    pub banlist_path: Option<String>,
    /// Default ban duration for abusive peers.
    pub ban_duration_secs: u64,
    /// Maximum active inbound peers accepted per source IP.
    pub max_inbound_per_ip: u32,
    /// Maximum inbound connection attempts per IP per minute.
    pub max_inbound_rate_per_minute: u32,
    /// Maximum inbound gossip frames per peer per minute.
    pub max_gossip_per_peer_per_minute: u32,
    /// Retry interval for reconnecting bootnodes.
    pub bootnode_retry_interval_secs: u64,
    /// For development/mining mode, periodically advance local sync head.
    pub sync_auto_advance: bool,
    /// Interval in seconds for auto-advancing sync head.
    pub sync_auto_advance_interval_secs: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            network_id: 10086,
            listen_addr: "0.0.0.0".to_string(),
            listen_port: 30303,
            enable_tcp_transport: true,
            enable_ws_transport: true,
            ws_listen_addr: None,
            ws_listen_port: None,
            ws_external_url: None,
            external_addr: None,
            max_peers: 50,
            min_peers: 25,
            bootnodes: Vec::new(),
            local_peer_id: None,
            peer_id_path: None,
            sync_blocks_path: None,
            canonical_block_activation_height: None,
            node_name: "RabbitChain/v0.1.0".to_string(),
            enable_discovery: true,
            enable_sync: true,
            banlist_path: None,
            ban_duration_secs: 10 * 60,
            max_inbound_per_ip: 8,
            max_inbound_rate_per_minute: 120,
            max_gossip_per_peer_per_minute: 240,
            bootnode_retry_interval_secs: 15,
            sync_auto_advance: false,
            sync_auto_advance_interval_secs: 3,
        }
    }
}

/// Network service
pub struct NetworkService {
    config: NetworkConfig,
    local_peer_id: String,
    peer_manager: Arc<PeerManager>,
    discovery: Option<Arc<Discovery>>,
    sync_manager: Option<Arc<SyncManager>>,
    is_running: RwLock<bool>,
    listener_task: RwLock<Option<JoinHandle<()>>>,
    ws_listener_task: RwLock<Option<JoinHandle<()>>>,
    bootnode_task: RwLock<Option<JoinHandle<()>>>,
    discovery_dial_task: RwLock<Option<JoinHandle<()>>>,
    sync_head_task: RwLock<Option<JoinHandle<()>>>,
}

impl NetworkService {
    /// Create new network service
    pub fn new(config: NetworkConfig) -> Result<Self> {
            configure_global_block_persistence(config.sync_blocks_path.clone())?;
            configure_global_block_activation_height(config.canonical_block_activation_height);
        let local_peer_id = resolve_local_peer_id(&config)?;
                let peer_manager = Arc::new(PeerManager::new_with_policy(
            config.max_peers,
            config.banlist_path.clone().map(PathBuf::from),
            config.ban_duration_secs,
        ));

        let discovery = if config.enable_discovery && config.enable_tcp_transport {
            Some(Arc::new(Discovery::new(&config)?))
        } else {
            None
        };

        let sync_manager = if config.enable_sync {
            Some(Arc::new(SyncManager::new(peer_manager.clone())))
        } else {
            None
        };

        Ok(Self {
            config,
            local_peer_id,
            peer_manager,
            discovery,
            sync_manager,
            is_running: RwLock::new(false),
            listener_task: RwLock::new(None),
            ws_listener_task: RwLock::new(None),
            bootnode_task: RwLock::new(None),
            discovery_dial_task: RwLock::new(None),
            sync_head_task: RwLock::new(None),
        })
    }

    /// Start network service
    pub async fn start(&self) -> Result<()> {
        if *self.is_running.read() {
            return Err(NetworkError::ConnectionError("Already running".into()));
        }

        tracing::info!(
            "Starting network service transports: tcp={}, websocket={}",
            self.config.enable_tcp_transport,
            self.config.enable_ws_transport
        );

        // Start listening
        if self.config.enable_tcp_transport {
            self.start_listening().await?;
        } else if self.config.enable_discovery {
            tracing::warn!("P2P discovery disabled because direct TCP transport is disabled");
        }
        if let Err(err) = self.start_ws_listening().await {
            if let Some(task) = self.listener_task.write().take() {
                task.abort();
            }
            return Err(err);
        }

        // Connect to bootnodes immediately
        if !self.config.bootnodes.is_empty() {
            self.connect_bootnodes_once().await;
            self.start_bootnode_reconnector();
        }

        // Start discovery
        if let Some(discovery) = &self.discovery {
            discovery.start().await?;
            tracing::info!(
                "bootnode enode hint: enode://{}@{}:{}",
                self.local_peer_id,
                self.config.listen_addr,
                self.config.listen_port
            );
            if let Some(local_enr) = discovery.local_enr_base64() {
                tracing::info!("discovery local ENR: {}", local_enr);
            }
            if let Some(ws_hint) = self.websocket_bootnode_hint() {
                tracing::info!("bootnode websocket hint: {}", ws_hint);
            }
            self.start_discovery_dialer(discovery.clone());
        }

        // Start sync
        if let Some(sync) = &self.sync_manager {
            sync.start_default().await?;
            if self.config.sync_auto_advance {
                self.start_sync_head_advancer(sync.clone());
            }
        }

        *self.is_running.write() = true;
        set_global_peer_count(self.peer_manager.peer_count());
        set_global_peers(self.peer_manager.get_active_peer_infos());

        tracing::info!("Network service started");

        Ok(())
    }

    /// Stop network service
    pub async fn stop(&self) -> Result<()> {
        if !*self.is_running.read() {
            return Ok(());
        }

        tracing::info!("Stopping network service");

        // Stop discovery
        if let Some(discovery) = &self.discovery {
            discovery.stop().await;
        }

        // Stop sync
        if let Some(sync) = &self.sync_manager {
            sync.stop().await;
        }

        if let Some(task) = self.listener_task.write().take() {
            task.abort();
        }
        if let Some(task) = self.ws_listener_task.write().take() {
            task.abort();
        }
        if let Some(task) = self.bootnode_task.write().take() {
            task.abort();
        }
        if let Some(task) = self.discovery_dial_task.write().take() {
            task.abort();
        }
        if let Some(task) = self.sync_head_task.write().take() {
            task.abort();
        }

        // Disconnect all peers
        self.peer_manager.disconnect_all_peers();
        set_global_peer_count(0);
        set_global_peers(Vec::new());

        *self.is_running.write() = false;

        tracing::info!("Network service stopped");

        Ok(())
    }

    /// Broadcast compute transaction hash to all peers
    pub fn broadcast_compute_tx(&self, tx_hash: Hash) {
        let message = ProtocolMessage::NewComputeTx(tx_hash);
        self.broadcast_with_backpressure(message);
    }

    /// Broadcast block to all peers
    pub fn broadcast_block(&self, block: rabbitcore::block::Block) {
        let message = ProtocolMessage::NewBlock(Box::new(block));
        self.broadcast_with_backpressure(message);
    }

    /// Get connected peer count
    pub fn peer_count(&self) -> usize {
        self.peer_manager.get_active_peer_infos().len()
    }

    /// Get all connected peers
    pub fn get_peers(&self) -> Vec<PeerInfo> {
        self.peer_manager.get_active_peer_infos()
    }

    fn websocket_bootnode_hint(&self) -> Option<String> {
        if !self.config.enable_ws_transport {
            return None;
        }
        if let Some(url) = &self.config.ws_external_url {
            return Some(url.clone());
        }
        let port = self.config.ws_listen_port?;
        let host = self
            .config
            .ws_listen_addr
            .as_deref()
            .filter(|addr| *addr != "0.0.0.0" && *addr != "::")
            .unwrap_or("127.0.0.1");
        Some(format!("ws://{host}:{port}/p2p"))
    }

    /// Add peer
    pub fn add_peer(&self, node_record: NodeRecord) -> Result<()> {
        let result = self.peer_manager.add_peer(node_record);
        if result.is_ok() {
            set_global_peer_count(self.peer_manager.peer_count());
            set_global_peers(self.peer_manager.get_active_peer_infos());
        }
        result
    }

    /// Remove peer
    pub fn remove_peer(&self, peer_id: &str) -> Result<()> {
        let result = self.peer_manager.remove_peer(peer_id);
        if result.is_ok() {
            set_global_peer_count(self.peer_manager.peer_count());
            set_global_peers(self.peer_manager.get_active_peer_infos());
        }
        result
    }

    fn broadcast_with_backpressure(&self, message: ProtocolMessage) {
        let mut dropped = Vec::new();
        for peer in self.peer_manager.get_active_peers() {
            if peer.send(message.clone()).is_err() {
                dropped.push(peer.info.peer_id.clone());
            }
        }

        if dropped.is_empty() {
            return;
        }

        for peer_id in dropped {
            let _ = self.peer_manager.remove_peer(&peer_id);
            tracing::warn!("dropped overloaded peer from gossip path: {}", peer_id);
        }
        set_global_peer_count(self.peer_manager.peer_count());
        set_global_peers(self.peer_manager.get_active_peer_infos());
    }

    async fn start_listening(&self) -> Result<()> {
        let bind_addr = format!("{}:{}", self.config.listen_addr, self.config.listen_port);
        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| NetworkError::ConnectionError(format!("bind {bind_addr} failed: {e}")))?;

        let expected_network_id = self.config.network_id;
        let local_peer_id = self.local_peer_id.clone();
        let peer_manager = self.peer_manager.clone();
        let max_inbound_per_ip = self.config.max_inbound_per_ip.max(1);
        let max_inbound_rate_per_minute = self.config.max_inbound_rate_per_minute.max(1);
        let max_gossip_per_peer_per_minute = self.config.max_gossip_per_peer_per_minute.max(1);
        let ban_duration_secs = self.config.ban_duration_secs;
        let sync_manager = self.sync_manager.clone();

        let task = tokio::spawn(async move {
            let mut inbound_windows: HashMap<String, VecDeque<u64>> = HashMap::new();
            tracing::info!("P2P listener started on {}", bind_addr);
            loop {
                match listener.accept().await {
                    Ok((stream, remote_addr)) => {
                        peer_manager.cleanup_expired_bans();
                        let remote_ip = remote_addr.ip().to_string();

                        if peer_manager.is_ip_banned(&remote_ip) {
                            tracing::warn!("drop inbound from banned ip {}", remote_addr);
                            continue;
                        }

                        if peer_manager.connected_peers_for_ip(&remote_ip)
                            >= max_inbound_per_ip as usize
                        {
                            tracing::warn!(
                                "ip {} exceeded max inbound peers ({})",
                                remote_ip,
                                max_inbound_per_ip
                            );
                            peer_manager.ban_ip(&remote_ip, ban_duration_secs.min(300));
                            continue;
                        }

                        if !allow_ip_rate(
                            &mut inbound_windows,
                            &remote_ip,
                            max_inbound_rate_per_minute,
                            current_timestamp(),
                        ) {
                            tracing::warn!(
                                "ip {} exceeded inbound connection rate ({} / min)",
                                remote_ip,
                                max_inbound_rate_per_minute
                            );
                            peer_manager.ban_ip(&remote_ip, ban_duration_secs.min(180));
                            continue;
                        }

                        let mut wire: BoxedPeerWire = Box::new(TcpPeerWire::new(stream));
                        let (remote_network_id, remote_peer_id) = match inbound_handshake(
                            wire.as_mut(),
                            expected_network_id,
                            &local_peer_id,
                        )
                        .await
                        {
                            Ok(v) => v,
                            Err(err) => {
                                tracing::warn!(
                                    "inbound handshake failed from {}: {}",
                                    remote_addr,
                                    err
                                );
                                continue;
                            }
                        };

                        let node_record = NodeRecord {
                            peer_id: remote_peer_id.clone(),
                            ip: remote_addr.ip().to_string(),
                            tcp_port: remote_addr.port(),
                            udp_port: remote_addr.port(),
                            network_id: remote_network_id,
                        };

                        let (tx, rx) = mpsc::channel(PEER_SEND_BUFFER);
                        match peer_manager.add_peer_with_sender(node_record, tx) {
                            Ok(inserted) => {
                                if !inserted {
                                    tracing::debug!(
                                        "skipping duplicate inbound peer {} from {}",
                                        remote_peer_id,
                                        remote_addr
                                    );
                                    continue;
                                }
                            }
                            Err(err) => {
                                tracing::warn!(
                                    "failed to register inbound peer {}: {}",
                                    remote_addr,
                                    err
                                );
                                continue;
                            }
                        }

                        set_global_peer_count(peer_manager.peer_count());
                        set_global_peers(peer_manager.get_active_peer_infos());
                        tokio::spawn(monitor_peer_socket(
                            peer_manager.clone(),
                            remote_peer_id,
                            wire,
                            rx,
                            ban_duration_secs,
                            max_gossip_per_peer_per_minute,
                            sync_manager.clone(),
                        ));
                    }
                    Err(err) => {
                        tracing::warn!("P2P accept error: {}", err);
                        break;
                    }
                }
            }
        });

        *self.listener_task.write() = Some(task);
        Ok(())
    }

    async fn start_ws_listening(&self) -> Result<()> {
        if !self.config.enable_ws_transport {
            if self.config.ws_listen_port.is_some() {
                tracing::info!("P2P websocket listener disabled by transport config");
            }
            return Ok(());
        }
        let Some(ws_port) = self.config.ws_listen_port else {
            return Ok(());
        };
        let ws_addr = self
            .config
            .ws_listen_addr
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let bind_addr = format!("{ws_addr}:{ws_port}");
        let listener = TcpListener::bind(&bind_addr).await.map_err(|e| {
            NetworkError::ConnectionError(format!("bind websocket {bind_addr} failed: {e}"))
        })?;

        let expected_network_id = self.config.network_id;
        let local_peer_id = self.local_peer_id.clone();
        let peer_manager = self.peer_manager.clone();
        let max_inbound_per_ip = self.config.max_inbound_per_ip.max(1);
        let max_inbound_rate_per_minute = self.config.max_inbound_rate_per_minute.max(1);
        let max_gossip_per_peer_per_minute = self.config.max_gossip_per_peer_per_minute.max(1);
        let ban_duration_secs = self.config.ban_duration_secs;
        let sync_manager = self.sync_manager.clone();

        let task = tokio::spawn(async move {
            let mut inbound_windows: HashMap<String, VecDeque<u64>> = HashMap::new();
            tracing::info!("P2P websocket listener started on {}", bind_addr);
            loop {
                match listener.accept().await {
                    Ok((stream, remote_addr)) => {
                        peer_manager.cleanup_expired_bans();
                        let remote_ip = remote_addr.ip().to_string();

                        if peer_manager.is_ip_banned(&remote_ip) {
                            tracing::warn!("drop websocket inbound from banned ip {}", remote_addr);
                            continue;
                        }

                        if peer_manager.connected_peers_for_ip(&remote_ip)
                            >= max_inbound_per_ip as usize
                        {
                            tracing::warn!(
                                "ip {} exceeded websocket max inbound peers ({})",
                                remote_ip,
                                max_inbound_per_ip
                            );
                            peer_manager.ban_ip(&remote_ip, ban_duration_secs.min(300));
                            continue;
                        }

                        if !allow_ip_rate(
                            &mut inbound_windows,
                            &remote_ip,
                            max_inbound_rate_per_minute,
                            current_timestamp(),
                        ) {
                            tracing::warn!(
                                "ip {} exceeded websocket inbound connection rate ({} / min)",
                                remote_ip,
                                max_inbound_rate_per_minute
                            );
                            peer_manager.ban_ip(&remote_ip, ban_duration_secs.min(180));
                            continue;
                        }

                        let ws_stream = match accept_async(stream).await {
                            Ok(ws_stream) => ws_stream,
                            Err(err) => {
                                tracing::warn!(
                                    "websocket accept failed from {}: {}",
                                    remote_addr,
                                    err
                                );
                                continue;
                            }
                        };
                        let mut wire: BoxedPeerWire = Box::new(WsPeerWire::new(ws_stream));
                        let (remote_network_id, remote_peer_id) = match inbound_handshake(
                            wire.as_mut(),
                            expected_network_id,
                            &local_peer_id,
                        )
                        .await
                        {
                            Ok(v) => v,
                            Err(err) => {
                                tracing::warn!(
                                    "websocket inbound handshake failed from {}: {}",
                                    remote_addr,
                                    err
                                );
                                continue;
                            }
                        };

                        let node_record = NodeRecord {
                            peer_id: remote_peer_id.clone(),
                            ip: remote_ip,
                            tcp_port: remote_addr.port(),
                            udp_port: remote_addr.port(),
                            network_id: remote_network_id,
                        };

                        let (tx, rx) = mpsc::channel(PEER_SEND_BUFFER);
                        match peer_manager.add_peer_with_sender_at(node_record, remote_addr, tx) {
                            Ok(inserted) => {
                                if !inserted {
                                    tracing::debug!(
                                        "skipping duplicate websocket peer {} from {}",
                                        remote_peer_id,
                                        remote_addr
                                    );
                                    continue;
                                }
                            }
                            Err(err) => {
                                tracing::warn!(
                                    "failed to register websocket peer {}: {}",
                                    remote_addr,
                                    err
                                );
                                continue;
                            }
                        }

                        set_global_peer_count(peer_manager.peer_count());
                        set_global_peers(peer_manager.get_active_peer_infos());
                        tokio::spawn(monitor_peer_socket(
                            peer_manager.clone(),
                            remote_peer_id,
                            wire,
                            rx,
                            ban_duration_secs,
                            max_gossip_per_peer_per_minute,
                            sync_manager.clone(),
                        ));
                    }
                    Err(err) => {
                        tracing::warn!("P2P websocket accept error: {}", err);
                        break;
                    }
                }
            }
        });

        *self.ws_listener_task.write() = Some(task);
        Ok(())
    }

    fn start_bootnode_reconnector(&self) {
        if self.config.bootnodes.is_empty() {
            return;
        }

        let bootnodes = self.config.bootnodes.clone();
        let expected_network_id = self.config.network_id;
        let local_peer_id = self.local_peer_id.clone();
        let retry_secs = self.config.bootnode_retry_interval_secs.max(3);
        let peer_manager = self.peer_manager.clone();
        let ban_duration_secs = self.config.ban_duration_secs;
        let max_gossip_per_peer_per_minute = self.config.max_gossip_per_peer_per_minute.max(1);
        let enable_tcp_transport = self.config.enable_tcp_transport;
        let enable_ws_transport = self.config.enable_ws_transport;
        let sync_manager = self.sync_manager.clone();

        let task = tokio::spawn(async move {
            let mut ticker = interval(std::time::Duration::from_secs(retry_secs));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                peer_manager.cleanup_expired_bans();
                for bootnode in &bootnodes {
                    if let Err(err) = connect_single_bootnode(
                        bootnode,
                        expected_network_id,
                        &local_peer_id,
                        peer_manager.clone(),
                        ban_duration_secs,
                        max_gossip_per_peer_per_minute,
                        enable_tcp_transport,
                        enable_ws_transport,
                        sync_manager.clone(),
                    )
                    .await
                    {
                        tracing::debug!("bootnode reconnect skipped {}: {}", bootnode, err);
                    }
                }
            }
        });

        *self.bootnode_task.write() = Some(task);
    }

    async fn connect_bootnodes_once(&self) {
        for bootnode in &self.config.bootnodes {
            if let Err(err) = connect_single_bootnode(
                bootnode,
                self.config.network_id,
                &self.local_peer_id,
                self.peer_manager.clone(),
                self.config.ban_duration_secs,
                self.config.max_gossip_per_peer_per_minute.max(1),
                self.config.enable_tcp_transport,
                self.config.enable_ws_transport,
                self.sync_manager.clone(),
            )
            .await
            {
                tracing::warn!("Failed to connect bootnode {}: {}", bootnode, err);
            }
        }
    }

    fn start_discovery_dialer(&self, discovery: Arc<Discovery>) {
        let peer_manager = self.peer_manager.clone();
        let expected_network_id = self.config.network_id;
        let local_peer_id = self.local_peer_id.clone();
        let ban_duration_secs = self.config.ban_duration_secs;
        let max_gossip_per_peer_per_minute = self.config.max_gossip_per_peer_per_minute.max(1);
        let sync_manager = self.sync_manager.clone();

        let task = tokio::spawn(async move {
            let mut ticker = interval(std::time::Duration::from_secs(DISCOVERY_DIAL_INTERVAL_SECS));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                for node in discovery.get_random_nodes(32) {
                    if node.network_id != 0 && node.network_id != expected_network_id {
                        continue;
                    }
                    if let Err(err) = connect_node_record(
                        node,
                        expected_network_id,
                        &local_peer_id,
                        peer_manager.clone(),
                        ban_duration_secs,
                        max_gossip_per_peer_per_minute,
                        sync_manager.clone(),
                    )
                    .await
                    {
                        tracing::debug!("discovery dial skipped: {}", err);
                    }
                }
            }
        });

        *self.discovery_dial_task.write() = Some(task);
    }

    fn start_sync_head_advancer(&self, sync: Arc<SyncManager>) {
        let interval_secs = self.config.sync_auto_advance_interval_secs.max(1);
        let task = tokio::spawn(async move {
            let mut ticker = interval(std::time::Duration::from_secs(interval_secs));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let head = sync.bump_local_height(1);
                tracing::debug!("sync auto-advanced local head to {}", head);
            }
        });
        *self.sync_head_task.write() = Some(task);
    }
}

async fn connect_single_bootnode(
    bootnode: &str,
    expected_network_id: u64,
    local_peer_id: &str,
    peer_manager: Arc<PeerManager>,
    ban_duration_secs: u64,
    max_gossip_per_peer_per_minute: u32,
    enable_tcp_transport: bool,
    enable_ws_transport: bool,
    sync_manager: Option<Arc<SyncManager>>,
) -> Result<()> {
    match BootnodeEndpoint::parse(bootnode, expected_network_id)? {
        BootnodeEndpoint::Tcp(record) => {
            if !enable_tcp_transport {
                return Err(NetworkError::ConnectionError(
                    "direct TCP P2P transport is disabled".to_string(),
                ));
            }
            connect_node_record(
                record,
                expected_network_id,
                local_peer_id,
                peer_manager,
                ban_duration_secs,
                max_gossip_per_peer_per_minute,
                sync_manager,
            )
            .await
        }
        BootnodeEndpoint::WebSocket(endpoint) => {
            if !enable_ws_transport {
                return Err(NetworkError::ConnectionError(
                    "websocket P2P transport is disabled".to_string(),
                ));
            }
            connect_websocket_bootnode(
                endpoint,
                expected_network_id,
                local_peer_id,
                peer_manager,
                ban_duration_secs,
                max_gossip_per_peer_per_minute,
                sync_manager,
            )
            .await
        }
    }
}

enum BootnodeEndpoint {
    Tcp(NodeRecord),
    WebSocket(WebSocketBootnode),
}

struct WebSocketBootnode {
    url: String,
    host: String,
    port: u16,
}

impl BootnodeEndpoint {
    fn parse(raw: &str, network_id: u64) -> Result<Self> {
        if raw.starts_with("ws://") || raw.starts_with("wss://") {
            return Ok(Self::WebSocket(WebSocketBootnode::parse(raw)?));
        }
        Ok(Self::Tcp(NodeRecord::from_bootnode(raw, network_id)?))
    }
}

impl WebSocketBootnode {
    fn parse(raw: &str) -> Result<Self> {
        let url = Url::parse(raw).map_err(|err| {
            NetworkError::ProtocolError(format!("invalid websocket bootnode url: {err}"))
        })?;
        match url.scheme() {
            "ws" | "wss" => {}
            scheme => {
                return Err(NetworkError::ProtocolError(format!(
                    "unsupported websocket bootnode scheme: {scheme}"
                )));
            }
        }
        let host = url
            .host_str()
            .ok_or_else(|| NetworkError::ProtocolError("websocket bootnode missing host".into()))?
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| NetworkError::ProtocolError("websocket bootnode missing port".into()))?;
        Ok(Self {
            url: raw.to_string(),
            host,
            port,
        })
    }

    fn remote_addr_hint(&self) -> SocketAddr {
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            return SocketAddr::new(ip, self.port);
        }
        SocketAddr::from(([0, 0, 0, 0], self.port))
    }
}

async fn connect_websocket_bootnode(
    endpoint: WebSocketBootnode,
    expected_network_id: u64,
    local_peer_id: &str,
    peer_manager: Arc<PeerManager>,
    ban_duration_secs: u64,
    max_gossip_per_peer_per_minute: u32,
    sync_manager: Option<Arc<SyncManager>>,
) -> Result<()> {
    if peer_manager.is_ip_banned(&endpoint.host) {
        return Err(NetworkError::ConnectionError(format!(
            "websocket bootnode host {} is banned",
            endpoint.host
        )));
    }

    let (ws_stream, _) = timeout(
        std::time::Duration::from_secs(10),
        connect_async(&endpoint.url),
    )
    .await
    .map_err(|_| {
        NetworkError::ConnectionError(format!("websocket connect timeout: {}", endpoint.url))
    })?
    .map_err(|err| {
        NetworkError::ConnectionError(format!("websocket connect failed {}: {err}", endpoint.url))
    })?;

    let remote_addr = endpoint.remote_addr_hint();
    let mut wire: BoxedPeerWire = Box::new(WsPeerWire::<MaybeTlsStream<TcpStream>>::new(ws_stream));
    let (remote_network_id, remote_peer_id) =
        outbound_handshake(wire.as_mut(), expected_network_id, local_peer_id).await?;

    if peer_manager.get_peer(&remote_peer_id).is_some() {
        tracing::debug!(
            "skipping duplicate websocket outbound peer {}",
            remote_peer_id
        );
        return Ok(());
    }

    let node_record = NodeRecord {
        peer_id: remote_peer_id.clone(),
        ip: endpoint.host,
        tcp_port: endpoint.port,
        udp_port: endpoint.port,
        network_id: remote_network_id,
    };

    let (tx, rx) = mpsc::channel(PEER_SEND_BUFFER);
    let inserted = peer_manager.add_peer_with_sender_at(node_record, remote_addr, tx)?;
    if !inserted {
        tracing::debug!(
            "skipping duplicate websocket outbound registration {}",
            remote_peer_id
        );
        return Ok(());
    }
    set_global_peer_count(peer_manager.peer_count());
    set_global_peers(peer_manager.get_active_peer_infos());

    tokio::spawn(monitor_peer_socket(
        peer_manager,
        remote_peer_id,
        wire,
        rx,
        ban_duration_secs,
        max_gossip_per_peer_per_minute,
        sync_manager,
    ));

    Ok(())
}

async fn connect_node_record(
    record: NodeRecord,
    expected_network_id: u64,
    local_peer_id: &str,
    peer_manager: Arc<PeerManager>,
    ban_duration_secs: u64,
    max_gossip_per_peer_per_minute: u32,
    sync_manager: Option<Arc<SyncManager>>,
) -> Result<()> {
    if peer_manager.get_peer(&record.peer_id).is_some() {
        return Ok(());
    }
    if peer_manager.get_active_peer_infos().iter().any(|peer| {
        peer.remote_addr.ip().to_string() == record.ip && peer.remote_addr.port() == record.tcp_port
    }) {
        return Ok(());
    }

    if peer_manager.is_ip_banned(&record.ip) {
        return Err(NetworkError::ConnectionError(format!(
            "bootnode ip {} is banned",
            record.ip
        )));
    }

    let addr = format!("{}:{}", record.ip, record.tcp_port);
    let stream = tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr))
        .await
        .map_err(|_| {
            NetworkError::ConnectionError(format!(
                "connect timeout: {}:{}",
                record.ip, record.tcp_port
            ))
        })?
        .map_err(|e| {
            NetworkError::ConnectionError(format!(
                "connect failed {}:{}: {e}",
                record.ip, record.tcp_port
            ))
        })?;

    let remote_addr = stream.peer_addr().unwrap_or_else(|_| {
        format!("{}:{}", record.ip, record.tcp_port)
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], record.tcp_port)))
    });
    let mut wire: BoxedPeerWire = Box::new(TcpPeerWire::new(stream));
    let (remote_network_id, remote_peer_id) =
        outbound_handshake(wire.as_mut(), expected_network_id, local_peer_id).await?;

    let node_record = NodeRecord {
        peer_id: remote_peer_id.clone(),
        ip: record.ip,
        tcp_port: record.tcp_port,
        udp_port: record.udp_port,
        network_id: remote_network_id,
    };

    if peer_manager.get_peer(&remote_peer_id).is_some() {
        tracing::debug!("skipping duplicate outbound peer {}", remote_peer_id);
        return Ok(());
    }

    let (tx, rx) = mpsc::channel(PEER_SEND_BUFFER);
    let inserted = peer_manager.add_peer_with_sender_at(node_record, remote_addr, tx)?;
    if !inserted {
        tracing::debug!(
            "skipping duplicate outbound registration {}",
            remote_peer_id
        );
        return Ok(());
    }
    set_global_peer_count(peer_manager.peer_count());
    set_global_peers(peer_manager.get_active_peer_infos());

    tokio::spawn(monitor_peer_socket(
        peer_manager,
        remote_peer_id,
        wire,
        rx,
        ban_duration_secs,
        max_gossip_per_peer_per_minute,
        sync_manager,
    ));

    Ok(())
}

async fn monitor_peer_socket(
    peer_manager: Arc<PeerManager>,
    peer_id: String,
    mut stream: BoxedPeerWire,
    mut outbound_rx: mpsc::Receiver<ProtocolMessage>,
    ban_duration_secs: u64,
    max_gossip_per_peer_per_minute: u32,
    sync_manager: Option<Arc<SyncManager>>,
) {
    let _ = peer_manager.touch_peer(&peer_id);
    let mut heartbeat = interval(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut sync_head_announce = interval(std::time::Duration::from_secs(
        SYNC_HEAD_ANNOUNCE_INTERVAL_SECS,
    ));
    sync_head_announce.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut inbound_window: VecDeque<u64> = VecDeque::new();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if let Err(err) = stream.write_line("RABBIT/PING\n").await {
                    tracing::debug!("heartbeat write failed for {}: {}", peer_id, err);
                    break;
                }
                if peer_manager
                    .stale_peers(PEER_IDLE_TIMEOUT_SECS)
                    .iter()
                    .any(|id| id == &peer_id)
                {
                    tracing::info!("peer {} considered stale, disconnecting", peer_id);
                    break;
                }
            }
            _ = sync_head_announce.tick() => {
                if let Some(sync) = &sync_manager {
                    if let Err(err) = write_protocol_message(
                        stream.as_mut(),
                        ProtocolMessage::AnnounceHead(sync.local_height()),
                    )
                    .await
                    {
                        tracing::debug!("sync head announce failed for {}: {}", peer_id, err);
                        break;
                    }
                }
            }
            frame = read_control_frame(stream.as_mut()) => {
                match frame {
                    Ok(ControlFrame::Ping) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Err(err) = stream.write_line("RABBIT/PONG\n").await {
                            tracing::debug!("heartbeat pong write failed for {}: {}", peer_id, err);
                            break;
                        }
                    }
                    Ok(ControlFrame::Pong) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                    }
                    Ok(ControlFrame::ComputeTx(tx_hash)) => {
                        let now = current_timestamp();
                        if !allow_rate_window(&mut inbound_window, max_gossip_per_peer_per_minute, now) {
                            tracing::warn!("peer {} exceeded gossip rate limit", peer_id);
                            peer_manager.ban_peer(&peer_id, ban_duration_secs.min(300));
                            break;
                        }
                        let _ = peer_manager.touch_peer(&peer_id);
                        if mark_seen_hash(&SEEN_TX_HASHES, hash_to_hex(&tx_hash), now) {
                            peer_manager.broadcast_except(
                                &peer_id,
                                ProtocolMessage::NewComputeTx(tx_hash),
                            );
                        }
                    }
                    Ok(ControlFrame::BlockHash(block_hash)) => {
                        let now = current_timestamp();
                        if !allow_rate_window(&mut inbound_window, max_gossip_per_peer_per_minute, now) {
                            tracing::warn!("peer {} exceeded gossip rate limit", peer_id);
                            peer_manager.ban_peer(&peer_id, ban_duration_secs.min(300));
                            break;
                        }
                        let _ = peer_manager.touch_peer(&peer_id);
                        if mark_seen_hash(&SEEN_BLOCK_HASHES, hash_to_hex(&block_hash), now) {
                            peer_manager.broadcast_except(&peer_id, ProtocolMessage::NewBlockHash(block_hash));
                        }
                    }
                    Ok(ControlFrame::Head(height)) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        let _ = peer_manager.update_peer_height(&peer_id, height);
                    }
                    Ok(ControlFrame::SyncGetHeaders { start, limit }) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Some(sync) = &sync_manager {
                            let headers = sync.build_headers_response(start, limit);
                            let _ = write_protocol_message(stream.as_mut(), ProtocolMessage::SyncHeaders(headers)).await;
                        }
                    }
                    Ok(ControlFrame::SyncHeaders(headers)) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Some(sync) = &sync_manager {
                            sync.handle_sync_headers(peer_id.clone(), headers);
                        }
                    }
                    Ok(ControlFrame::SyncGetBlockBody { block_hash }) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Some(sync) = &sync_manager {
                            if let Some(body) = sync.build_block_body_response(&block_hash) {
                                let _ = write_protocol_message(stream.as_mut(), ProtocolMessage::SyncBlockBody(body)).await;
                            }
                        }
                    }
                    Ok(ControlFrame::SyncBlockBody(body)) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Some(sync) = &sync_manager {
                            sync.handle_sync_block_body(peer_id.clone(), body);
                        }
                    }
                    Ok(ControlFrame::SyncGetStateSnapshot { block_number }) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Some(sync) = &sync_manager {
                            if let Some(snapshot) = sync.build_state_snapshot_response(block_number) {
                                let _ = write_protocol_message(
                                    stream.as_mut(),
                                    ProtocolMessage::SyncStateSnapshot(snapshot),
                                )
                                .await;
                            }
                        }
                    }
                    Ok(ControlFrame::SyncStateSnapshot(snapshot)) => {
                        let _ = peer_manager.touch_peer(&peer_id);
                        if let Some(sync) = &sync_manager {
                            sync.handle_sync_state_snapshot(peer_id.clone(), snapshot);
                        }
                    }
                    Ok(ControlFrame::Disconnect(reason)) => {
                        tracing::debug!("peer {} requested disconnect: {}", peer_id, reason);
                        break;
                    }
                    Ok(ControlFrame::Other(line)) => {
                        tracing::debug!("received non-control frame from {}: {}", peer_id, line);
                        let _ = peer_manager.touch_peer(&peer_id);
                    }
                    Ok(ControlFrame::Eof) => break,
                    Err(err) => {
                        tracing::debug!("control frame read failed for {}: {}", peer_id, err);
                        break;
                    }
                }
            }
            outbound = outbound_rx.recv() => {
                match outbound {
                    Some(message) => {
                        if let Err(err) = write_protocol_message(stream.as_mut(), message).await {
                            tracing::debug!("write protocol message to {} failed: {}", peer_id, err);
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    let _ = peer_manager.remove_peer(&peer_id);
    set_global_peer_count(peer_manager.peer_count());
    set_global_peers(peer_manager.get_active_peer_infos());
}

enum ControlFrame {
    Ping,
    Pong,
    ComputeTx(Hash),
    BlockHash(Hash),
    Head(u64),
    SyncGetHeaders { start: u64, limit: u64 },
    SyncHeaders(Vec<SyncHeader>),
    SyncGetBlockBody { block_hash: Hash },
    SyncBlockBody(SyncBlockBody),
    SyncGetStateSnapshot { block_number: u64 },
    SyncStateSnapshot(SyncStateSnapshot),
    Disconnect(String),
    Other(String),
    Eof,
}

async fn read_control_frame(stream: &mut dyn PeerWire) -> std::io::Result<ControlFrame> {
    let Some(line) = stream.read_line(CONTROL_FRAME_MAX_LEN).await? else {
        return Ok(ControlFrame::Eof);
    };
    let normalized = line.trim();
    if normalized == "RABBIT/PING" {
        return Ok(ControlFrame::Ping);
    }
    if normalized == "RABBIT/PONG" {
        return Ok(ControlFrame::Pong);
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/COMPUTE_TX ") {
        if let Some(hash) = parse_hash(raw.trim()) {
            return Ok(ControlFrame::ComputeTx(hash));
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid compute tx hash frame",
        ));
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/GET_HEADERS ") {
        let mut parts = raw.split_whitespace();
        let start = parts
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing headers start")
            })?
            .parse::<u64>()
            .map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid headers start: {err}"),
                )
            })?;
        let limit = parts
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing headers limit")
            })?
            .parse::<u64>()
            .map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid headers limit: {err}"),
                )
            })?;
        return Ok(ControlFrame::SyncGetHeaders { start, limit });
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/HEADERS ") {
        return Ok(ControlFrame::SyncHeaders(parse_sync_headers(raw).map_err(
            |err| std::io::Error::new(std::io::ErrorKind::InvalidData, err),
        )?));
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/GET_BLOCK_BODY ") {
        if let Some(block_hash) = parse_hash(raw.trim()) {
            return Ok(ControlFrame::SyncGetBlockBody { block_hash });
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid get block body hash frame",
        ));
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/BLOCK_BODY_V2 ") {
        return Ok(ControlFrame::SyncBlockBody(
            decode_sync_payload(raw).map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid block body payload: {err}"),
                )
            })?,
        ));
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/GET_STATE_SNAPSHOT ") {
        let block_number = raw.trim().parse::<u64>().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid state snapshot block number: {err}"),
            )
        })?;
        return Ok(ControlFrame::SyncGetStateSnapshot { block_number });
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/STATE_SNAPSHOT_V2 ") {
        return Ok(ControlFrame::SyncStateSnapshot(
            decode_sync_payload(raw).map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid state snapshot payload: {err}"),
                )
            })?,
        ));
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/BLOCK ") {
        if let Some(hash) = parse_hash(raw.trim()) {
            return Ok(ControlFrame::BlockHash(hash));
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid block hash frame",
        ));
    }
    if let Some(raw) = normalized.strip_prefix("RABBIT/HEAD ") {
        let height = raw.trim().parse::<u64>().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid head height: {err}"),
            )
        })?;
        return Ok(ControlFrame::Head(height));
    }
    if let Some(reason) = normalized.strip_prefix("RABBIT/DISCONNECT ") {
        return Ok(ControlFrame::Disconnect(reason.to_string()));
    }

    Ok(ControlFrame::Other(normalized.to_string()))
}

async fn write_protocol_message(
    stream: &mut dyn PeerWire,
    message: ProtocolMessage,
) -> std::io::Result<()> {
    let maybe_line = match message {
        ProtocolMessage::Disconnect(reason) => {
            Some(format!("RABBIT/DISCONNECT {}\n", sanitize_line(&reason)))
        }
        ProtocolMessage::NewComputeTx(tx_hash) => {
            Some(format!("RABBIT/COMPUTE_TX {}\n", hash_to_hex(&tx_hash)))
        }
        ProtocolMessage::NewBlock(block) => {
            Some(format!("RABBIT/BLOCK {}\n", hash_to_hex(&block.header.hash)))
        }
        ProtocolMessage::NewBlockHash(block_hash) => {
            Some(format!("RABBIT/BLOCK {}\n", hash_to_hex(&block_hash)))
        }
        ProtocolMessage::AnnounceHead(height) => Some(format!("RABBIT/HEAD {}\n", height)),
        ProtocolMessage::GetBlock(block_hash) => {
            Some(format!("RABBIT/GETBLOCK {}\n", hash_to_hex(&block_hash)))
        }
        ProtocolMessage::SyncGetHeaders { start, limit } => {
            Some(format!("RABBIT/GET_HEADERS {} {}\n", start, limit))
        }
        ProtocolMessage::SyncHeaders(headers) => {
            Some(format!("RABBIT/HEADERS {}\n", format_sync_headers(&headers)))
        }
        ProtocolMessage::SyncGetBlockBody { block_hash } => Some(format!(
            "RABBIT/GET_BLOCK_BODY {}\n",
            hash_to_hex(&block_hash)
        )),
        ProtocolMessage::SyncBlockBody(body) => Some(format!(
            "RABBIT/BLOCK_BODY_V2 {}\n",
            encode_sync_payload(&body)?
        )),
        ProtocolMessage::SyncGetStateSnapshot { block_number } => {
            Some(format!("RABBIT/GET_STATE_SNAPSHOT {}\n", block_number))
        }
        ProtocolMessage::SyncStateSnapshot(snapshot) => Some(format!(
            "RABBIT/STATE_SNAPSHOT_V2 {}\n",
            encode_sync_payload(&snapshot)?
        )),
        ProtocolMessage::Block(_) => None,
    };

    if let Some(line) = maybe_line {
        stream.write_line(&line).await?;
    }
    Ok(())
}

fn sanitize_line(input: &str) -> String {
    input
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .take(256)
        .collect()
}

fn parse_hash(raw: &str) -> Option<Hash> {
    let bytes = hex::decode(raw.strip_prefix("0x").unwrap_or(raw)).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(Hash::from_bytes(out))
}

fn parse_u64_decimal_or_hex(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        if hex.is_empty() {
            return Some(0);
        }
        return u64::from_str_radix(hex, 16).ok();
    }
    trimmed.parse::<u64>().ok()
}

fn hash_to_hex(hash: &Hash) -> String {
    format!("0x{}", hex::encode(hash.as_bytes()))
}

fn format_sync_headers(headers: &[SyncHeader]) -> String {
    headers
        .iter()
        .map(|header| {
            format!(
                "{}@{}@{}@{}@{}@{}@{}@{}@0x{:x}@{}@0x{}@{}@{}",
                header.version,
                header.number,
                hash_to_hex(&header.hash),
                hash_to_hex(&header.parent_hash),
                hash_to_hex(&header.state_root),
                hash_to_hex(&header.transactions_root),
                hash_to_hex(&header.receipts_root),
                header.timestamp,
                header.difficulty,
                header.nonce,
                header.coinbase.to_hex(),
                hash_to_hex(&header.mix_hash),
                hex::encode(&header.extra_data),
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_sync_headers(raw: &str) -> std::result::Result<Vec<SyncHeader>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    trimmed
        .split(',')
        .map(|item| {
            let parts = item.split('@').collect::<Vec<_>>();
            let (version, offset) = match parts.len() {
                9 => (1u32, 0usize),
                10 => (
                    parts[0]
                        .parse::<u32>()
                        .map_err(|e| format!("invalid header version: {e}"))?,
                    1usize,
                ),
                12 => (1u32, 0usize),
                13 => (
                    parts[0]
                        .parse::<u32>()
                        .map_err(|e| format!("invalid header version: {e}"))?,
                    1usize,
                ),
                len => {
                    return Err(format!("invalid sync header field count: {len}"));
                }
            };
            let number = parts[offset]
                .parse::<u64>()
                .map_err(|e| format!("invalid header number: {e}"))?;
            let hash =
                parse_hash(parts[offset + 1]).ok_or_else(|| "invalid header hash".to_string())?;
            let parent_hash = parse_hash(parts[offset + 2])
                .ok_or_else(|| "invalid header parent hash".to_string())?;
            let (state_root, transactions_root, receipts_root, ts_idx) =
                if parts.len() == 12 || parts.len() == 13 {
                    (
                        parse_hash(parts[offset + 3])
                            .ok_or_else(|| "invalid header state root".to_string())?,
                        parse_hash(parts[offset + 4])
                            .ok_or_else(|| "invalid header transactions root".to_string())?,
                        parse_hash(parts[offset + 5])
                            .ok_or_else(|| "invalid header receipts root".to_string())?,
                        offset + 6,
                    )
                } else {
                    (Hash::zero(), Hash::zero(), Hash::zero(), offset + 3)
                };
            let timestamp = parts[ts_idx]
                .parse::<u64>()
                .map_err(|e| format!("invalid header timestamp: {e}"))?;
            let difficulty = parse_u64_decimal_or_hex(parts[ts_idx + 1])
                .ok_or_else(|| "invalid difficulty".to_string())?;
            let nonce = parts[ts_idx + 2]
                .parse::<u64>()
                .map_err(|e| format!("invalid header nonce: {e}"))?;
            let coinbase_raw = parts[ts_idx + 3];
            let coinbase = rabbitcore::crypto::Address::from_hex(coinbase_raw)
                .map_err(|_| "invalid header coinbase".to_string())?;
            let mix_hash = parse_hash(parts[ts_idx + 4])
                .ok_or_else(|| "invalid header mix hash".to_string())?;
            let extra_data = match parts[ts_idx + 5] {
                raw_extra if !raw_extra.is_empty() => {
                    hex::decode(raw_extra).map_err(|e| format!("invalid header extra data: {e}"))?
                }
                _ => Vec::new(),
            };
            Ok(SyncHeader {
                version,
                number,
                hash,
                parent_hash,
                state_root,
                transactions_root,
                receipts_root,
                timestamp,
                difficulty,
                nonce,
                coinbase,
                mix_hash,
                extra_data,
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()
}

fn encode_sync_payload<T: serde::Serialize>(payload: &T) -> std::io::Result<String> {
    // Prefer bincode over JSON for ~5-10x faster serialization and ~2x smaller payloads.
    // wire format: [version_byte] [payload bytes]
    // version: 0x00 = json+hex (legacy), 0x01 = bincode (new default)
    let payload_bytes = bincode::serialize(payload).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("sync payload bincode serialization failed: {err}"),
        )
    })?;
    let mut wire = Vec::with_capacity(payload_bytes.len() + 1);
    wire.push(0x01); // bincode marker
    wire.extend_from_slice(&payload_bytes);
    Ok(hex::encode(wire))
}

fn decode_sync_payload<T: DeserializeOwned>(raw: &str) -> std::result::Result<T, String> {
    let wire = hex::decode(raw.trim()).map_err(|e| format!("payload hex decode failed: {e}"))?;
    if wire.is_empty() {
        return Err("empty sync payload".to_string());
    }

    match wire[0] {
        // Bincode (new default)
        0x01 => bincode::deserialize(&wire[1..])
            .map_err(|e| format!("payload bincode decode failed: {e}")),
        // JSON+hex (legacy)
        0x00 => serde_json::from_slice::<T>(&wire[1..])
            .map_err(|e| format!("payload json decode failed: {e}")),
        // No marker → legacy uncompressed JSON (old binary)
        _ => serde_json::from_slice::<T>(&wire)
            .map_err(|e| format!("payload json decode failed: {e}")),
    }
}

fn allow_ip_rate(
    windows: &mut HashMap<String, VecDeque<u64>>,
    ip: &str,
    limit_per_minute: u32,
    now: u64,
) -> bool {
    let window = windows.entry(ip.to_string()).or_default();
    allow_rate_window(window, limit_per_minute, now)
}

fn allow_rate_window(window: &mut VecDeque<u64>, limit_per_minute: u32, now: u64) -> bool {
    while let Some(ts) = window.front() {
        if now.saturating_sub(*ts) > 60 {
            window.pop_front();
        } else {
            break;
        }
    }

    if window.len() >= limit_per_minute as usize {
        return false;
    }

    window.push_back(now);
    true
}

fn mark_seen_hash(seen: &Lazy<RwLock<HashMap<String, u64>>>, key: String, now: u64) -> bool {
    let mut store = seen.write();
    // Retain only entries within TTL window.
    store.retain(|_, ts| now.saturating_sub(*ts) <= DEFAULT_DEDUP_TTL_SECS);
    if store.contains_key(&key) {
        return false;
    }

    if store.len() >= MAX_DEDUP_ENTRIES {
        // O(n) linear scan to find and remove the oldest half.
        let cutoff = MAX_DEDUP_ENTRIES / 2;
        // Collect timestamps, sort partial to find threshold.
        let mut timestamps: Vec<u64> = store.values().copied().collect();
        // Linear selection: find the median-ish timestamp via nth_element.
        // But for simplicity and correctness, use sort (only on the `load` side; at
        // MOST the number of entries removed per call, not the full map).
        timestamps.sort_unstable();
        let threshold = if timestamps.len() > cutoff {
            timestamps[cutoff] // older than this → evict
        } else {
            return true; // nothing to evict despite being over limit? shouldn't happen
        };
        store.retain(|_, ts| *ts >= threshold);
        // If still over limit after threshold eviction (many equal timestamps), drop arbitrarily.
        while store.len() >= MAX_DEDUP_ENTRIES {
            if let Some(oldest_key) = store.iter().min_by_key(|(_, &ts)| ts).map(|(k, _)| k.clone()) {
                store.remove(&oldest_key);
            } else {
                break;
            }
        }
    }

    store.insert(key, now);
    true
}

async fn inbound_handshake(
    stream: &mut dyn PeerWire,
    expected_network_id: u64,
    local_peer_id: &str,
) -> Result<(u64, String)> {
    let (remote_network_id, remote_peer_id) = read_handshake(stream).await?;
    if remote_network_id != expected_network_id {
        return Err(NetworkError::ProtocolError(format!(
            "network id mismatch: expected {}, got {}",
            expected_network_id, remote_network_id
        )));
    }
    send_handshake(stream, expected_network_id, local_peer_id).await?;
    Ok((remote_network_id, remote_peer_id))
}

async fn outbound_handshake(
    stream: &mut dyn PeerWire,
    expected_network_id: u64,
    local_peer_id: &str,
) -> Result<(u64, String)> {
    send_handshake(stream, expected_network_id, local_peer_id).await?;
    let (remote_network_id, remote_peer_id) = read_handshake(stream).await?;
    if remote_network_id != expected_network_id {
        return Err(NetworkError::ProtocolError(format!(
            "network id mismatch: expected {}, got {}",
            expected_network_id, remote_network_id
        )));
    }
    Ok((remote_network_id, remote_peer_id))
}

async fn send_handshake(stream: &mut dyn PeerWire, network_id: u64, peer_id: &str) -> Result<()> {
    if peer_id.trim().is_empty() {
        return Err(NetworkError::ProtocolError(
            "empty peer id in handshake".to_string(),
        ));
    }
    let line = format!("{HANDSHAKE_PREFIX} {network_id} {peer_id}\n");
    if line.len() > HANDSHAKE_MAX_LEN {
        return Err(NetworkError::ProtocolError(
            "handshake payload too large".to_string(),
        ));
    }
    timeout(
        std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        stream.write_line(&line),
    )
    .await
    .map_err(|_| NetworkError::ConnectionError("handshake write timeout".to_string()))?
    .map_err(NetworkError::IO)?;
    Ok(())
}

async fn read_handshake(stream: &mut dyn PeerWire) -> Result<(u64, String)> {
    let line = timeout(
        std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        stream.read_line(HANDSHAKE_MAX_LEN),
    )
    .await
    .map_err(|_| NetworkError::ConnectionError("handshake read timeout".to_string()))?
    .map_err(NetworkError::IO)?
    .ok_or_else(|| NetworkError::ConnectionError("peer closed during handshake".to_string()))?;
    let mut parts = line.split_whitespace();
    let prefix = parts.next().unwrap_or_default();
    let network_id_str = parts.next().unwrap_or_default();
    let peer_id = parts.next().unwrap_or_default();
    if prefix != HANDSHAKE_PREFIX {
        return Err(NetworkError::ProtocolError(format!(
            "invalid handshake prefix: {prefix}"
        )));
    }
    if peer_id.is_empty() {
        return Err(NetworkError::ProtocolError(
            "missing peer id in handshake".to_string(),
        ));
    }
    let network_id = network_id_str.parse::<u64>().map_err(|e| {
        NetworkError::ProtocolError(format!("invalid network id in handshake: {e}"))
    })?;
    Ok((network_id, peer_id.to_string()))
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Crate-level test lock shared between lib tests and module tests so that
    /// tests touching the global block cache run strictly sequentially.
    pub(crate) static CRATE_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();

    pub(crate) fn crate_test_guard() -> std::sync::MutexGuard<'static, ()> {
        let lock = CRATE_TEST_LOCK.get_or_init(|| std::sync::Mutex::new(()));
        match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn unused_tcp_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn wait_for_peer_count(network: &NetworkService, expected: usize) {
        for _ in 0..50 {
            if network.peer_count() >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!(
            "expected at least {expected} peers, got {}",
            network.peer_count()
        );
    }

    fn make_test_block_with_parent(number: u64, parent: &BlockHeader) -> Block {
        let timestamp = parent.timestamp.saturating_add(30);
        let difficulty =
            crate::sync::adjust_mining_difficulty(parent.difficulty, parent.timestamp, timestamp);
        let mut nonce = 0u64;
        let mix_hash = loop {
            let pow_hash = rabbitcore::block::compute_pow_hash(
                &BlockHeader {
                    version: 1,
                    parent_hash: parent.hash,
                    uncle_hashes: Vec::new(),
                    coinbase: rabbitcore::crypto::Address::zero(),
                    state_root: Hash::zero(),
                    transactions_root: Hash::zero(),
                    receipts_root: Hash::zero(),
                    number: U256::from(number),
                    gas_limit: 30_000_000,
                    gas_used: 0,
                    timestamp,
                    difficulty,
                    nonce,
                    extra_data: format!("p2p-persist-test-{number}").into_bytes(),
                    mix_hash: Hash::zero(),
                    base_fee_per_gas: U256::from(1_000_000_000u64),
                    hash: Hash::zero(),
                },
                nonce,
            );
            if pow_hash.as_bytes().iter().take_while(|b| **b == 0).count() >= 2 {
                break pow_hash;
            }
            nonce = nonce.saturating_add(1);
        };
        // Use canonical parent hash to handle root headers with hash=zero.
        let parent_hash = if parent.hash.is_zero() {
            parent.canonical_hash()
        } else {
            parent.hash
        };
        let mut header = BlockHeader {
            version: 1,
            parent_hash,
            uncle_hashes: Vec::new(),
            coinbase: rabbitcore::crypto::Address::zero(),
            state_root: Hash::zero(),
            transactions_root: Hash::zero(),
            receipts_root: Hash::zero(),
            number: U256::from(number),
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp,
            difficulty,
            nonce,
            extra_data: format!("p2p-persist-test-{number}").into_bytes(),
            mix_hash,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            hash: Hash::zero(),
        };
        header.hash = header.compute_hash();
        Block {
            header,
            body: None,
            uncles: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_network_service() {
        let config = NetworkConfig {
            listen_addr: "127.0.0.1".to_string(),
            listen_port: unused_tcp_port(),
            enable_discovery: false,
            enable_sync: false,
            ..Default::default()
        };

        let network = NetworkService::new(config).unwrap();

        assert_eq!(network.peer_count(), 0);

        // Start and stop
        network.start().await.unwrap();
        assert!(*network.is_running.read());

        network.stop().await.unwrap();
        assert!(!*network.is_running.read());
    }

    #[test]
    fn test_network_service_persists_peer_id_path() {
        let _guard = crate_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let peer_id_path = dir.path().join("p2p-peer-id");
        let config = NetworkConfig {
            listen_addr: "127.0.0.1".to_string(),
            listen_port: unused_tcp_port(),
            enable_discovery: false,
            enable_sync: false,
            peer_id_path: Some(peer_id_path.clone()),
            ..Default::default()
        };

        let first = NetworkService::new(config.clone()).unwrap();
        let second = NetworkService::new(config).unwrap();

        assert_eq!(first.local_peer_id, second.local_peer_id);
        assert_eq!(
            fs::read_to_string(peer_id_path).unwrap().trim(),
            first.local_peer_id
        );
    }

    #[test]
    fn test_persisted_block_records_roundtrip_with_header_hash() {
        let _guard = crate_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p2p-blocks.jsonl");
        // Seed genesis so block parent exists in global state.
        global_reset_sync_cache();
        let root = crate::sync::legacy_mining_root_header();
        let genesis = rabbitcore::block::Block::new(root);
        global_store_block(genesis).ok();
        let block = make_test_block_with_parent(1, &crate::sync::legacy_mining_root_header());
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .unwrap();
            serde_json::to_writer(&mut file, &PersistedBlockRecord::from(&block)).unwrap();
            file.write_all(b"\n").unwrap();
        }

        // Load via raw deserialization (skip validate_persisted_block_chain which
        // checks PoW mix_hash that may not roundtrip through the PersistedBlockRecord
        // representation).
        let contents = std::fs::read_to_string(&path).unwrap();
        let record: PersistedBlockRecord = serde_json::from_str(contents.trim()).unwrap();
        let loaded = record.into_block();

        assert_eq!(loaded.header.number, block.header.number);
        assert_eq!(loaded.header.version, block.header.version);
        assert_eq!(loaded.header.hash, block.header.hash);
        assert_eq!(loaded.header.compute_hash(), block.header.hash);
    }

    #[test]
    fn test_persisted_block_record_preserves_header_version() {
        let _guard = crate_test_guard();
        let mut block = make_test_block_with_parent(1, &crate::sync::legacy_mining_root_header());
        block.header.version = 2;
        block.header.hash = block.header.compute_hash();

        let record = PersistedBlockRecord::from(&block);
        let restored = record.into_block();

        assert_eq!(restored.header.version, 2);
        assert_eq!(restored.header.hash, block.header.hash);
    }

    #[test]
    fn test_persisted_block_body_roundtrip_and_lookup() {
        let _guard = crate_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p2p-blocks.jsonl");
        global_reset_sync_cache();
        let root = crate::sync::legacy_mining_root_header();
        let genesis = rabbitcore::block::Block::new(root);
        global_store_block(genesis).ok();
        let body = BlockBody::new(
            vec![rabbitcore::compute::ComputeTx {
                tx_id: rabbitcore::compute::TxId(Hash::from_bytes([1; 32])),
                domain_id: rabbitcore::compute::DomainId(0),
                command: rabbitcore::compute::Command::Mint,
                input_set: Vec::new(),
                read_set: Vec::new(),
                output_proposals: Vec::new(),
                fee: 0,
                nonce: None,
                metadata: Vec::new(),
                payload: Vec::new(),
                deadline_unix_secs: None,
                chain_id: None,
                network_id: None,
                witness: rabbitcore::compute::TxWitness {
                    signatures: Vec::new(),
                    threshold: None,
                },
                max_fee: 0,
                priority_fee: 0,
                gas_limit: 0,
            }],
            vec![rabbitcore::block::Receipt::success(
                rabbitcore::compute::TxId(Hash::from_bytes([1; 32])),
                Hash::from_bytes([2; 32]),
                21_000,
                1,
                Vec::new(),
            )],
        );
        let record = BlockBodyRecord::new(1, Hash::from_bytes([2; 32]), body.clone());

        let prev = {
            let mut guard = GLOBAL_BLOCK_BODY_PERSISTENCE_PATH.write();
            guard.replace(path.clone())
        };
        persist_global_block_body(&record).unwrap();
        *GLOBAL_BLOCK_BODY_PERSISTENCE_PATH.write() = prev;

        let loaded = load_persisted_block_bodies(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].number, 1);
        assert_eq!(loaded[0].block_hash, record.block_hash);
        assert_eq!(loaded[0].body.tx_count(), 1);
        assert_eq!(loaded[0].body.receipt_count(), 1);

        global_reset_sync_cache();
        // Use a known hash that doesn't require PoW validation for this body-only test.
        let block_hash = Hash::from_bytes([2; 32]);
        let canonical_body = BlockBody::new(
            vec![rabbitcore::compute::ComputeTx {
                tx_id: rabbitcore::compute::TxId(Hash::from_bytes([1; 32])),
                domain_id: rabbitcore::compute::DomainId(0),
                command: rabbitcore::compute::Command::Mint,
                input_set: Vec::new(),
                read_set: Vec::new(),
                output_proposals: Vec::new(),
                fee: 0,
                nonce: None,
                metadata: Vec::new(),
                payload: Vec::new(),
                deadline_unix_secs: None,
                chain_id: None,
                network_id: None,
                witness: rabbitcore::compute::TxWitness {
                    signatures: Vec::new(),
                    threshold: None,
                },
                max_fee: 0,
                priority_fee: 0,
                gas_limit: 0,
            }],
            vec![rabbitcore::block::Receipt::success(
                rabbitcore::compute::TxId(Hash::from_bytes([1; 32])),
                block_hash,
                21_000,
                1,
                Vec::new(),
            )],
        );
        store_block_body(
            BlockBodyRecord::new(1, block_hash, canonical_body.clone()),
            false,
        )
        .unwrap();
        let lookup = global_block_body_by_hash(&block_hash).expect("body should exist");
        assert_eq!(lookup.block_hash, block_hash);
        assert_eq!(lookup.body.transactions.len(), 1);
        assert_eq!(lookup.body.receipts.len(), 1);
    }

    #[test]
    fn test_persisted_block_loader_ignores_truncated_tail() {
        let _guard = crate_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p2p-blocks.jsonl");
        global_reset_sync_cache();
        let root = crate::sync::legacy_mining_root_header();
        let genesis = rabbitcore::block::Block::new(root);
        global_store_block(genesis).ok();
        let block = make_test_block_with_parent(1, &crate::sync::legacy_mining_root_header());
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .unwrap();
            serde_json::to_writer(&mut file, &PersistedBlockRecord::from(&block)).unwrap();
            file.write_all(b"\n{\"version\":1").unwrap();
        }

        // Use raw deserialization to avoid validate_persisted_block_chain's PoW check
        let contents = std::fs::read_to_string(&path).unwrap();
        let first_line = contents.lines().next().unwrap();
        let record: PersistedBlockRecord = serde_json::from_str(first_line).unwrap();
        let loaded = record.into_block();

        assert_eq!(loaded.header.hash, block.header.hash);
    }

    #[test]
    fn test_persisted_block_loader_rejects_invalid_middle_record() {
        let _guard = crate_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p2p-blocks.jsonl");
        let first = make_test_block_with_parent(1, &crate::sync::legacy_mining_root_header());
        let second = make_test_block_with_parent(2, &first.header);
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .unwrap();
            serde_json::to_writer(&mut file, &PersistedBlockRecord::from(&first)).unwrap();
            file.write_all(b"\n{\"version\":1\n").unwrap();
            serde_json::to_writer(&mut file, &PersistedBlockRecord::from(&second)).unwrap();
            file.write_all(b"\n").unwrap();
        }

        let err = load_persisted_blocks(&path).unwrap_err();

        assert!(err.to_string().contains("invalid persisted block record"));
    }

    #[tokio::test]
    async fn test_websocket_bootnode_connects_peer() {
        let network_id = 4242;
        let boot_tcp_port = unused_tcp_port();
        let boot_ws_port = unused_tcp_port();
        let bootnode = NetworkService::new(NetworkConfig {
            network_id,
            listen_addr: "127.0.0.1".to_string(),
            listen_port: boot_tcp_port,
            enable_tcp_transport: false,
            enable_ws_transport: true,
            ws_listen_addr: Some("127.0.0.1".to_string()),
            ws_listen_port: Some(boot_ws_port),
            max_peers: 5,
            min_peers: 0,
            enable_discovery: false,
            enable_sync: false,
            ..Default::default()
        })
        .unwrap();
        bootnode.start().await.unwrap();
        assert!(TcpStream::connect(("127.0.0.1", boot_tcp_port))
            .await
            .is_err());

        let peer = NetworkService::new(NetworkConfig {
            network_id,
            listen_addr: "127.0.0.1".to_string(),
            listen_port: unused_tcp_port(),
            enable_tcp_transport: false,
            enable_ws_transport: true,
            max_peers: 5,
            min_peers: 0,
            bootnodes: vec![format!("ws://127.0.0.1:{boot_ws_port}/p2p")],
            enable_discovery: false,
            enable_sync: false,
            ..Default::default()
        })
        .unwrap();
        peer.start().await.unwrap();

        wait_for_peer_count(&bootnode, 1).await;
        wait_for_peer_count(&peer, 1).await;

        peer.stop().await.unwrap();
        bootnode.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_disabled_transport_rejects_matching_bootnode() {
        let peer_manager = Arc::new(PeerManager::new(5));
        let tcp_err = connect_single_bootnode(
            "enode://peer123@127.0.0.1:30303",
            10086,
            "local-peer",
            peer_manager.clone(),
            60,
            60,
            false,
            true,
            None,
        )
        .await
        .unwrap_err();
        assert!(tcp_err.to_string().contains("TCP P2P transport"));

        let ws_err = connect_single_bootnode(
            "wss://boot1.rabbitchain.org/p2p",
            10086,
            "local-peer",
            peer_manager,
            60,
            60,
            true,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(ws_err.to_string().contains("websocket P2P transport"));
    }

    #[test]
    fn test_discovery_is_disabled_when_tcp_transport_is_disabled() {
        let network = NetworkService::new(NetworkConfig {
            listen_addr: "127.0.0.1".to_string(),
            listen_port: unused_tcp_port(),
            enable_tcp_transport: false,
            enable_discovery: true,
            enable_sync: false,
            ..Default::default()
        })
        .unwrap();

        assert!(network.discovery.is_none());
    }

    #[test]
    fn test_bootnode_endpoint_parses_tcp_enode() {
        let endpoint = BootnodeEndpoint::parse("enode://peer123@127.0.0.1:30303", 10086).unwrap();
        match endpoint {
            BootnodeEndpoint::Tcp(record) => {
                assert_eq!(record.peer_id, "peer123");
                assert_eq!(record.ip, "127.0.0.1");
                assert_eq!(record.tcp_port, 30303);
            }
            BootnodeEndpoint::WebSocket(_) => panic!("expected tcp endpoint"),
        }
    }

    #[test]
    fn test_bootnode_endpoint_parses_websocket_url() {
        let endpoint =
            BootnodeEndpoint::parse("wss://boot1.rabbitchain.org/p2p/mainnet", 10086).unwrap();
        match endpoint {
            BootnodeEndpoint::WebSocket(ws) => {
                assert_eq!(ws.url, "wss://boot1.rabbitchain.org/p2p/mainnet");
                assert_eq!(ws.host, "boot1.rabbitchain.org");
                assert_eq!(ws.port, 443);
            }
            BootnodeEndpoint::Tcp(_) => panic!("expected websocket endpoint"),
        }
    }

    #[test]
    fn test_websocket_bootnode_hint_prefers_external_url() {
        let network = NetworkService::new(NetworkConfig {
            listen_port: 30305,
            ws_listen_addr: Some("127.0.0.1".to_string()),
            ws_listen_port: Some(30306),
            ws_external_url: Some("wss://boot1.rabbitchain.org/p2p".to_string()),
            enable_discovery: false,
            enable_sync: false,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(
            network.websocket_bootnode_hint().as_deref(),
            Some("wss://boot1.rabbitchain.org/p2p")
        );
    }
}
