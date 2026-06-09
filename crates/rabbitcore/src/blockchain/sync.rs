//! Block synchronization

use super::{Blockchain, BlockchainError, Result};
use crate::block::{Block, BlockHeader};
use crate::crypto::Hash;
use crate::account::U256;
use parking_lot::RwLock;
use std::sync::Arc;
use std::collections::VecDeque;

/// Sync mode
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncMode {
    /// Full sync (download all blocks)
    Full,
    /// Fast sync (download headers + recent state)
    Fast,
    /// Light sync (headers only)
    Light,
}

/// Sync state
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Not syncing
    Idle,
    /// Syncing
    Syncing {
        /// Starting block
        start: u64,
        /// Current block
        current: u64,
        /// Target block
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
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            mode: SyncMode::Fast,
            min_peers: 3,
            batch_size: 100,
            max_pending: 1000,
        }
    }
}

/// Sync statistics
#[derive(Clone, Debug, Default)]
pub struct SyncStats {
    /// Is syncing
    pub is_syncing: bool,
    /// Blocks downloaded
    pub blocks_downloaded: u64,
    /// Blocks processed
    pub blocks_processed: u64,
    /// Headers downloaded
    pub headers_downloaded: u64,
    /// Sync start time
    pub start_time: u64,
    /// Last activity
    pub last_activity: u64,
}

/// Block sync manager
pub struct SyncManager {
    /// Configuration
    config: SyncConfig,
    /// Blockchain reference
    blockchain: Arc<Blockchain>,
    /// Sync state
    state: RwLock<SyncState>,
    /// Sync statistics
    stats: RwLock<SyncStats>,
    /// Pending blocks to process
    pending_blocks: RwLock<VecDeque<Block>>,
    /// Known peers
    peers: RwLock<Vec<String>>,
}

impl SyncManager {
    /// Create new sync manager
    pub fn new(config: SyncConfig, blockchain: Arc<Blockchain>) -> Self {
        Self {
            config,
            blockchain,
            state: RwLock::new(SyncState::Idle),
            stats: RwLock::new(SyncStats::default()),
            pending_blocks: RwLock::new(VecDeque::new()),
            peers: RwLock::new(Vec::new()),
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
        
        *self.state.write() = SyncState::Syncing {
            start: current,
            current,
            target: target_block,
        };
        
        let mut stats = self.stats.write();
        stats.is_syncing = true;
        stats.start_time = current_timestamp();
        
        Ok(())
    }
    
    /// Stop sync
    pub fn stop_sync(&self) {
        tracing::info!("Stopping sync");
        
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
        self.stats.read().clone()
    }
    
    /// Check if syncing
    pub fn is_syncing(&self) -> bool {
        matches!(*self.state.read(), SyncState::Syncing { .. })
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
    
    /// Download block headers
    pub async fn download_headers(
        &self,
        from: u64,
        limit: u64,
    ) -> Result<Vec<BlockHeader>> {
        // Would request from peers
        // Simplified
        Ok(Vec::new())
    }
    
    /// Download blocks
    pub async fn download_blocks(
        &self,
        hashes: Vec<Hash>,
    ) -> Result<Vec<Block>> {
        // Would request from peers
        // Simplified
        Ok(Vec::new())
    }
    
    /// Process downloaded block
    pub fn process_block(&self, block: Block) -> Result<()> {
        // Validate and insert block
        let is_new_best = self.blockchain.insert_block(block.clone())?;
        
        // Update sync state
        if let SyncState::Syncing { current, target, .. } = *self.state.read() {
            let new_current = block.header.number.as_u64();
            
            *self.state.write() = if new_current >= target {
                SyncState::Complete
            } else {
                SyncState::Syncing {
                    start: current,
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
    
    /// Get sync progress
    pub fn get_progress(&self) -> f64 {
        match *self.state.read() {
            SyncState::Syncing { start, current, target } => {
                if target <= start {
                    1.0
                } else {
                    (current - start) as f64 / (target - start) as f64
                }
            }
            SyncState::Idle => 0.0,
            SyncState::Complete => 1.0,
        }
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
    use crate::consensus::PowConsensus;
    use crate::consensus::PowAlgorithm;
    use crate::state::StateDb;
    
    #[test]
    fn test_sync_manager() {
        let config = SyncConfig::default();
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));
        
        let sync = SyncManager::new(config, blockchain);
        
        assert!(!sync.is_syncing());
        assert_eq!(sync.get_progress(), 0.0);
    }
    
    #[test]
    fn test_sync_progress() {
        let config = SyncConfig::default();
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus, state_db));
        
        let sync = SyncManager::new(config, blockchain);
        
        // Set syncing state
        *sync.state.write() = SyncState::Syncing {
            start: 0,
            current: 50,
            target: 100,
        };
        
        assert_eq!(sync.get_progress(), 0.5);
    }
}
