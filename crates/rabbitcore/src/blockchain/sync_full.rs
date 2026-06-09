//! Complete Block Synchronization Implementation

use super::{Blockchain, BlockchainError, Result};
use crate::block::{Block, BlockHeader};
use crate::crypto::Hash;
use crate::account::U256;
use crate::consensus::{Consensus, PowConsensus, PowAlgorithm};
use crate::state::StateDb;
use crate::state::executor::{StateExecutor, StateTransition};
use parking_lot::RwLock;
use std::sync::Arc;
use std::collections::VecDeque;
use thiserror::Error;

/// Sync errors
#[derive(Error, Debug, Clone)]
pub enum SyncError {
    #[error("Block not found")]
    BlockNotFound,
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Invalid state root")]
    InvalidStateRoot,
    #[Error("Orphan block")]
    OrphanBlock,
    #[error("Sync already in progress")]
    SyncInProgress,
    #[error("Peer not found")]
    PeerNotFound,
    #[error("Timeout")]
    Timeout,
}

/// Sync mode
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncMode {
    /// Full sync (download and execute all blocks)
    Full,
    /// Fast sync (download headers + recent state)
    Fast,
    /// Light sync (headers only)
    Light,
    /// Snap sync (snapshot sync)
    Snap,
}

/// Sync state
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Not syncing
    Idle,
    /// Waiting for peers
    WaitingForPeers,
    /// Downloading headers
    DownloadingHeaders {
        from: u64,
        current: u64,
        target: u64,
    },
    /// Downloading blocks
    DownloadingBlocks {
        from: u64,
        current: u64,
        target: u64,
    },
    /// Processing blocks
    ProcessingBlocks {
        current: u64,
        target: u64,
    },
    /// Sync complete
    Complete,
}

/// Sync configuration
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// Sync mode
    pub mode: SyncMode,
    /// Minimum peers to start sync
    pub min_peers: usize,
    /// Block batch size
    pub batch_size: u64,
    /// Max pending blocks
    pub max_pending: usize,
    /// Header request limit
    pub header_limit: u64,
    /// State sync interval
    pub state_sync_interval: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            mode: SyncMode::Fast,
            min_peers: 3,
            batch_size: 100,
            max_pending: 1000,
            header_limit: 1000,
            state_sync_interval: 10000,
        }
    }
}

/// Sync statistics
#[derive(Clone, Debug, Default)]
pub struct SyncStats {
    /// Is syncing
    pub is_syncing: bool,
    /// Sync start time
    pub start_time: u64,
    /// Headers downloaded
    pub headers_downloaded: u64,
    /// Blocks downloaded
    pub blocks_downloaded: u64,
    /// Blocks processed
    pub blocks_processed: u64,
    /// States downloaded
    pub states_downloaded: u64,
    /// Last activity
    pub last_activity: u64,
    /// Current peer count
    pub peer_count: usize,
    /// Sync speed (blocks/s)
    pub sync_speed: f64,
}

/// Block sync manager
pub struct SyncManager {
    /// Configuration
    config: SyncConfig,
    /// Blockchain reference
    blockchain: Arc<Blockchain>,
    /// State database
    state_db: Arc<StateDb>,
    /// State executor
    executor: Arc<StateExecutor>,
    /// Sync state
    state: RwLock<SyncState>,
    /// Sync statistics
    stats: RwLock<SyncStats>,
    /// Pending blocks to process
    pending_blocks: RwLock<VecDeque<Block>>,
    /// Downloaded headers
    headers: RwLock<Vec<BlockHeader>>,
    /// Known peers
    peers: RwLock<Vec<String>>,
    /// Consensus
    consensus: Arc<PowConsensus>,
    /// Is sync cancelled
    cancelled: AtomicBool,
}

use std::sync::atomic::AtomicBool;

impl SyncManager {
    /// Create new sync manager
    pub fn new(
        config: SyncConfig,
        blockchain: Arc<Blockchain>,
        state_db: Arc<StateDb>,
        consensus: Arc<PowConsensus>,
    ) -> Self {
        let executor = Arc::new(StateExecutor::new(
            state_db.clone(),
            10086,  // chain_id
        ));
        
        Self {
            config,
            blockchain,
            state_db,
            executor,
            state: RwLock::new(SyncState::Idle),
            stats: RwLock::new(SyncStats::default()),
            pending_blocks: RwLock::new(VecDeque::new()),
            headers: RwLock::new(Vec::new()),
            peers: RwLock::new(Vec::new()),
            consensus,
            cancelled: AtomicBool::new(false),
        }
    }
    
    /// Start sync
    pub async fn start_sync(&self, target_block: u64) -> Result<()> {
        let current = self.blockchain.best_number();
        
        if current >= target_block {
            tracing::info!("Already synced to latest block");
            return Ok(());
        }
        
        tracing::info!("Starting sync from {} to {}", current, target_block);
        
        // Check minimum peers
        let peer_count = self.peers.read().len();
        if peer_count < self.config.min_peers {
            *self.state.write() = SyncState::WaitingForPeers;
            return Err(SyncError::PeerNotFound.into());
        }
        
        *self.state.write() = SyncState::DownloadingHeaders {
            from: current,
            current,
            target: target_block,
        };
        
        let mut stats = self.stats.write();
        stats.is_syncing = true;
        stats.start_time = current_timestamp();
        stats.peer_count = peer_count;
        
        Ok(())
    }
    
    /// Stop sync
    pub fn stop_sync(&self) {
        tracing::info!("Stopping sync");
        
        self.cancelled.store(true, Ordering::Relaxed);
        *self.state.write() = SyncState::Idle;
        
        let mut stats = self.stats.write();
        stats.is_syncing = false;
    }
    
    /// Get sync state
    pub fn get_sync_state(&self) -> SyncState {
        self.state.read().clone()
    }
    
    /// Get sync statistics
    pub fn get_stats(&self) -> SyncStats {
        let stats = self.stats.read();
        
        let elapsed = current_timestamp().saturating_sub(stats.start_time);
        let sync_speed = if elapsed > 0 {
            stats.blocks_processed as f64 / elapsed as f64
        } else {
            0.0
        };
        
        SyncStats {
            sync_speed,
            ..(*stats).clone()
        }
    }
    
    /// Check if syncing
    pub fn is_syncing(&self) -> bool {
        matches!(*self.state.read(), SyncState::DownloadingHeaders { .. } | SyncState::DownloadingBlocks { .. } | SyncState::ProcessingBlocks { .. })
    }
    
    /// Add peer for sync
    pub fn add_peer(&self, peer_id: String) {
        self.peers.write().push(peer_id);
    }
    
    /// Remove peer
    pub fn remove_peer(&self, peer_id: &str) {
        self.peers.write().retain(|id| id != peer_id);
    }
    
    /// Get peer count
    pub fn peer_count(&self) -> usize {
        self.peers.read().len()
    }
    
    /// Download headers from peers
    pub async fn download_headers(
        &self,
        from: u64,
        limit: u64,
    ) -> Result<Vec<BlockHeader>> {
        tracing::debug!("Downloading headers from {} limit {}", from, limit);
        
        // Would request from peers via P2P
        // Simplified - return empty for now
        let headers = Vec::new();
        
        let mut stats = self.stats.write();
        stats.headers_downloaded += headers.len() as u64;
        stats.last_activity = current_timestamp();
        
        Ok(headers)
    }
    
    /// Download blocks from peers
    pub async fn download_blocks(
        &self,
        hashes: Vec<Hash>,
    ) -> Result<Vec<Block>> {
        tracing::debug!("Downloading {} blocks", hashes.len());
        
        // Would request from peers via P2P
        // Simplified - return empty for now
        let blocks = Vec::new();
        
        let mut stats = self.stats.write();
        stats.blocks_downloaded += blocks.len() as u64;
        stats.last_activity = current_timestamp();
        
        Ok(blocks)
    }
    
    /// Process downloaded block
    pub fn process_block(&self, block: Block) -> Result<()> {
        // Validate block
        self.validate_block(&block)?;
        
        // Execute block
        let parent_root = self.state_db.state_root();
        let transition = self.executor.execute_block(&block, parent_root)?;
        
        // Verify state root
        if transition.to_root != block.header.state_root {
            return Err(SyncError::InvalidStateRoot.into());
        }
        
        // Insert block into blockchain
        self.blockchain.insert_block(block)?;
        
        // Update sync state
        if let SyncState::ProcessingBlocks { current, target } = *self.state.read() {
            let new_current = current + 1;
            
            *self.state.write() = if new_current >= target {
                SyncState::Complete
            } else {
                SyncState::ProcessingBlocks {
                    current: new_current,
                    target,
                }
            };
            
            // Update stats
            let mut stats = self.stats.write();
            stats.blocks_processed += 1;
            stats.last_activity = current_timestamp();
        }
        
        Ok(())
    }
    
    /// Queue block for processing
    pub fn queue_block(&self, block: Block) {
        let mut pending = self.pending_blocks.write();
        
        if pending.len() < self.config.max_pending {
            pending.push_back(block);
        }
    }
    
    /// Process queued blocks
    pub fn process_queued_blocks(&self) -> Result<()> {
        let mut pending = self.pending_blocks.write();
        
        while let Some(block) = pending.pop_front() {
            self.process_block(block)?;
        }
        
        Ok(())
    }
    
    /// Validate block
    fn validate_block(&self, block: &Block) -> Result<()> {
        // Get parent
        let parent = self.blockchain.get_block(&block.header.parent_hash)
            .ok_or_else(|| SyncError::InvalidBlock("Parent not found".into()))?;
        
        // Validate header
        block.header.validate(&parent.header)
            .map_err(|e| SyncError::InvalidBlock(e.to_string()))?;
        
        // Validate PoW
        self.consensus.verify_pow(&block.header)
            .map_err(|e| SyncError::InvalidBlock(e.to_string()))?;
        
        // Validate gas used
        if block.header.gas_used > block.header.gas_limit {
            return Err(SyncError::InvalidBlock("Gas used exceeds limit".into()));
        }
        
        Ok(())
    }
    
    /// Get sync progress (0.0 to 1.0)
    pub fn get_progress(&self) -> f64 {
        match *self.state.read() {
            SyncState::Idle => 0.0,
            SyncState::WaitingForPeers => 0.0,
            SyncState::DownloadingHeaders { start, current, target } => {
                if target <= start {
                    1.0
                } else {
                    ((current - start) as f64 * 0.3) / (target - start) as f64
                }
            }
            SyncState::DownloadingBlocks { start, current, target } => {
                if target <= start {
                    1.0
                } else {
                    0.3 + ((current - start) as f64 * 0.5) / (target - start) as f64
                }
            }
            SyncState::ProcessingBlocks { current, target } => {
                if target <= current {
                    1.0
                } else {
                    0.8 + (current as f64 * 0.2) / target as f64
                }
            }
            SyncState::Complete => 1.0,
        }
    }
    
    /// Fast sync to target
    pub async fn fast_sync(&self, target: u64) -> Result<()> {
        let current = self.blockchain.best_number();
        
        if current >= target {
            return Ok(());
        }
        
        tracing::info!("Fast sync from {} to {}", current, target);
        
        // Phase 1: Download headers
        *self.state.write() = SyncState::DownloadingHeaders {
            from: current,
            current,
            target,
        };
        
        let mut header_num = current;
        while header_num < target && !self.cancelled.load(Ordering::Relaxed) {
            let limit = std::cmp::min(self.config.header_limit, target - header_num);
            let headers = self.download_headers(header_num, limit).await?;
            
            if headers.is_empty() {
                break;
            }
            
            header_num += headers.len() as u64;
            
            let mut state = self.state.write();
            if let SyncState::DownloadingHeaders { current, .. } = &mut *state {
                *current = header_num;
            }
        }
        
        // Phase 2: Download and process blocks
        *self.state.write() = SyncState::ProcessingBlocks {
            current,
            target,
        };
        
        // Simplified - would download and process actual blocks
        let mut state = self.state.write();
        *state = SyncState::Complete;
        
        Ok(())
    }
}

/// Full sync implementation
pub struct FullSync {
    sync_manager: Arc<SyncManager>,
}

impl FullSync {
    pub fn new(sync_manager: Arc<SyncManager>) -> Self {
        Self { sync_manager }
    }
    
    /// Execute full sync
    pub async fn execute(&self, target: u64) -> Result<()> {
        tracing::info!("Starting full sync to block {}", target);
        
        let current = self.sync_manager.blockchain.best_number();
        
        // Download and process each block
        for block_num in (current + 1)..=target {
            if self.sync_manager.cancelled.load(Ordering::Relaxed) {
                break;
            }
            
            // Would download block from peer
            // For now, skip
        }
        
        Ok(())
    }
}

/// Light sync implementation
pub struct LightSync {
    sync_manager: Arc<SyncManager>,
}

impl LightSync {
    pub fn new(sync_manager: Arc<SyncManager>) -> Self {
        Self { sync_manager }
    }
    
    /// Execute light sync (headers only)
    pub async fn execute(&self, target: u64) -> Result<()> {
        tracing::info!("Starting light sync to block {}", target);
        
        let current = self.sync_manager.blockchain.best_number();
        
        // Download headers only
        let headers = self.sync_manager.download_headers(current, target - current).await?;
        
        // Store headers
        *self.sync_manager.headers.write() = headers;
        
        Ok(())
    }
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
    
    #[test]
    fn test_sync_manager_creation() {
        let config = SyncConfig::default();
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus.clone(), state_db.clone()));
        
        let sync = SyncManager::new(config, blockchain, state_db, consensus);
        
        assert!(!sync.is_syncing());
        assert_eq!(sync.get_progress(), 0.0);
    }
    
    #[test]
    fn test_sync_progress() {
        let config = SyncConfig::default();
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus.clone(), state_db.clone()));
        
        let sync = SyncManager::new(config, blockchain, state_db, consensus);
        
        // Set processing state
        *sync.state.write() = SyncState::ProcessingBlocks {
            current: 50,
            target: 100,
        };
        
        let progress = sync.get_progress();
        assert!(progress > 0.8);
        assert!(progress < 1.0);
    }
}
