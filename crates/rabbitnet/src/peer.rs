//! Peer management module

use crate::protocol::ProtocolMessage;
use crate::{set_global_peer_count, set_global_peers, NetworkError, Result};
use parking_lot::RwLock;
use rabbitcore::crypto::Hash;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Peer ID
pub type PeerId = String;

/// Peer information
#[derive(Clone, Debug)]
pub struct PeerInfo {
    /// Peer ID
    pub peer_id: PeerId,
    /// Client version
    pub client_version: String,
    /// Protocol version
    pub protocol_version: u32,
    /// Network ID
    pub network_id: u64,
    /// Remote address
    pub remote_addr: SocketAddr,
    /// Local address
    pub local_addr: SocketAddr,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Connection time
    pub connected_at: u64,
    /// Last activity
    pub last_activity: u64,
    /// Reputation score
    pub reputation: i32,
}

impl PeerInfo {
    pub fn new(
        peer_id: PeerId,
        remote_addr: SocketAddr,
        local_addr: SocketAddr,
        network_id: u64,
    ) -> Self {
        let now = current_timestamp();

        Self {
            peer_id,
            client_version: "Unknown".to_string(),
            protocol_version: 1,
            network_id,
            remote_addr,
            local_addr,
            capabilities: Vec::new(),
            connected_at: now,
            last_activity: now,
            reputation: 100,
        }
    }

    /// Update last activity
    pub fn update_activity(&mut self) {
        self.last_activity = current_timestamp();
    }

    /// Increase reputation
    pub fn increase_reputation(&mut self, amount: i32) {
        self.reputation = (self.reputation + amount).min(1000);
    }

    /// Decrease reputation
    pub fn decrease_reputation(&mut self, amount: i32) {
        self.reputation = (self.reputation - amount).max(-1000);
    }
}

/// Peer connection status
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerStatus {
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
}

/// Peer connection
pub struct Peer {
    /// Peer info
    pub info: PeerInfo,
    /// Status
    pub status: PeerStatus,
    /// Send channel
    tx: mpsc::Sender<ProtocolMessage>,
    /// Known compute operations
    known_transactions: RwLock<HashSet<Hash>>,
}

impl Peer {
    /// Create new peer
    pub fn new(info: PeerInfo, tx: mpsc::Sender<ProtocolMessage>) -> Self {
        Self {
            info,
            status: PeerStatus::Connected,
            tx,
            known_transactions: RwLock::new(HashSet::new()),
        }
    }

    /// Send message to peer
    pub fn send(&self, message: ProtocolMessage) -> Result<()> {
        self.tx
            .try_send(message)
            .map_err(|_| NetworkError::ChannelError)?;

        Ok(())
    }

    /// Check if peer knows compute operation
    pub fn knows_operation(&self, tx_hash: &Hash) -> bool {
        self.known_transactions.read().contains(tx_hash)
    }

    /// Mark compute operation as known
    pub fn mark_operation_known(&self, tx_hash: Hash) {
        let mut known = self.known_transactions.write();

        // Limit size
        if known.len() > 10000 {
            known.clear();
        }

        known.insert(tx_hash);
    }

    /// Update activity timestamp
    pub fn update_activity(&self) {
        // Updated by peer manager
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedBanList {
    peers: HashMap<PeerId, u64>,
    ips: HashMap<String, u64>,
}

/// Peer manager
pub struct PeerManager {
    /// Maximum peers
    max_peers: u32,
    /// Connected peers
    peers: RwLock<HashMap<PeerId, Arc<Peer>>>,
    /// Last activity timestamps by peer
    activity: RwLock<HashMap<PeerId, u64>>,
    /// Last advertised peer heights
    heights: RwLock<HashMap<PeerId, u64>>,
    /// Peer scores
    scores: RwLock<HashMap<PeerId, i32>>,
    /// Outbound/inbound monitor task handles (aborted on remove/stop).
    peer_tasks: RwLock<HashMap<PeerId, tokio::task::JoinHandle<()>>>,
    /// Persisted banlist path
    banlist_path: Option<PathBuf>,
    /// Default ban duration in seconds
    default_ban_duration_secs: u64,
    /// Temporarily banned peers (peer_id -> expires_at)
    banned_peers: RwLock<HashMap<PeerId, u64>>,
    /// Temporarily banned IPs (ip -> expires_at)
    banned_ips: RwLock<HashMap<String, u64>>,
}

impl PeerManager {
    /// Create new peer manager
    pub fn new(max_peers: u32) -> Self {
        Self::new_with_policy(max_peers, None, 600)
    }

    /// Create peer manager with ban policy and optional persistence file.
    pub fn new_with_policy(
        max_peers: u32,
        banlist_path: Option<PathBuf>,
        default_ban_duration_secs: u64,
    ) -> Self {
        let (persisted_peer_bans, persisted_ip_bans) =
            load_persisted_bans(banlist_path.as_ref()).unwrap_or_default();

        Self {
            max_peers,
            peers: RwLock::new(HashMap::new()),
            activity: RwLock::new(HashMap::new()),
            heights: RwLock::new(HashMap::new()),
            scores: RwLock::new(HashMap::new()),
            peer_tasks: RwLock::new(HashMap::new()),
            banlist_path,
            default_ban_duration_secs,
            banned_peers: RwLock::new(persisted_peer_bans),
            banned_ips: RwLock::new(persisted_ip_bans),
        }
    }

    /// Add peer with an externally managed outbound message sender.
    pub fn add_peer_with_sender(
        &self,
        node_record: crate::discovery::NodeRecord,
        tx: mpsc::Sender<ProtocolMessage>,
    ) -> Result<bool> {
        let remote_addr: SocketAddr = format!("{}:{}", node_record.ip, node_record.tcp_port)
            .parse()
            .map_err(|e| {
                NetworkError::ConnectionError(format!("Invalid peer address format: {e}"))
            })?;
        self.add_peer_with_sender_at(node_record, remote_addr, tx)
    }

    /// Add peer using an explicit socket address for non-IP transports such as WebSocket/CDN.
    pub fn add_peer_with_sender_at(
        &self,
        node_record: crate::discovery::NodeRecord,
        remote_addr: SocketAddr,
        tx: mpsc::Sender<ProtocolMessage>,
    ) -> Result<bool> {
        self.cleanup_expired_bans();

        if self.peers.read().len() >= self.max_peers as usize {
            return Err(NetworkError::ConnectionError("Max peers reached".into()));
        }

        if self.is_banned(&node_record.peer_id) {
            return Err(NetworkError::ConnectionError(format!(
                "peer {} is banned",
                node_record.peer_id
            )));
        }

        if self.is_ip_banned(&node_record.ip) {
            return Err(NetworkError::ConnectionError(format!(
                "ip {} is banned",
                node_record.ip
            )));
        }

        let peer_id = node_record.peer_id.clone();
        if self.peers.read().contains_key(&peer_id) {
            return Ok(false);
        }

        let local_addr = SocketAddr::from(([0, 0, 0, 0], 0));

        let info = PeerInfo::new(
            peer_id.clone(),
            remote_addr,
            local_addr,
            node_record.network_id,
        );
        let peer = Arc::new(Peer::new(info, tx));

        self.peers.write().insert(peer_id.clone(), peer);
        self.activity
            .write()
            .insert(peer_id.clone(), current_timestamp());
        self.heights.write().insert(peer_id.clone(), 0);
        self.scores.write().entry(peer_id).or_insert(0);
        set_global_peer_count(self.peer_count());
        set_global_peers(self.get_active_peer_infos());

        Ok(true)
    }

    /// Remove peer
    pub fn remove_peer(&self, peer_id: &str) -> Result<()> {
        self.peers.write().remove(peer_id);
        self.activity.write().remove(peer_id);
        self.heights.write().remove(peer_id);
        self.scores.write().remove(peer_id);
        if let Some(handle) = self.peer_tasks.write().remove(peer_id) {
            handle.abort();
        }
        set_global_peer_count(self.peer_count());
        set_global_peers(self.get_active_peer_infos());
        Ok(())
    }

    /// Register the monitor task for a peer so stop/remove can abort it.
    pub fn register_peer_task(&self, peer_id: &str, handle: tokio::task::JoinHandle<()>) {
        if let Some(prev) = self.peer_tasks.write().insert(peer_id.to_string(), handle) {
            prev.abort();
        }
    }

    /// Abort and clear all registered peer tasks (used on network stop).
    pub fn abort_all_peer_tasks(&self) {
        let mut tasks = self.peer_tasks.write();
        for (_, handle) in tasks.drain() {
            handle.abort();
        }
    }

    /// Get peer by ID
    pub fn get_peer(&self, peer_id: &str) -> Option<Arc<Peer>> {
        self.peers.read().get(peer_id).cloned()
    }

    /// Get all active peers
    pub fn get_active_peers(&self) -> Vec<Arc<Peer>> {
        self.peers
            .read()
            .values()
            .filter(|p| p.status == PeerStatus::Connected)
            .cloned()
            .collect()
    }

    /// Get active peer infos
    pub fn get_active_peer_infos(&self) -> Vec<PeerInfo> {
        let peers = self.peers.read();
        let activity = self.activity.read();
        peers
            .values()
            .filter(|p| p.status == PeerStatus::Connected)
            .map(|p| {
                let mut info = p.info.clone();
                if let Some(ts) = activity.get(&info.peer_id) {
                    info.last_activity = *ts;
                }
                info
            })
            .collect()
    }

    /// Get peer count
    pub fn peer_count(&self) -> usize {
        self.peers.read().len()
    }

    /// Count currently connected peers for a specific remote IP.
    pub fn connected_peers_for_ip(&self, ip: &str) -> usize {
        self.peers
            .read()
            .values()
            .filter(|peer| peer.info.remote_addr.ip().to_string() == ip)
            .count()
    }

    /// Disconnect all peers
    pub fn disconnect_all_peers(&self) {
        for peer in self.peers.read().values() {
            let _ = peer.send(ProtocolMessage::Disconnect("Shutting down".into()));
        }

        self.abort_all_peer_tasks();
        self.peers.write().clear();
        self.activity.write().clear();
        self.heights.write().clear();
        set_global_peer_count(0);
        set_global_peers(Vec::new());
    }

    /// Get best peers for sync (height first, then score)
    pub fn get_best_peers(&self, limit: usize) -> Vec<Arc<Peer>> {
        let mut peers: Vec<_> = self.peers.read().values().cloned().collect();

        peers.sort_by(|a, b| {
            let height_a = self
                .heights
                .read()
                .get(&a.info.peer_id)
                .copied()
                .unwrap_or(0);
            let height_b = self
                .heights
                .read()
                .get(&b.info.peer_id)
                .copied()
                .unwrap_or(0);
            let score_a = self
                .scores
                .read()
                .get(&a.info.peer_id)
                .copied()
                .unwrap_or(0);
            let score_b = self
                .scores
                .read()
                .get(&b.info.peer_id)
                .copied()
                .unwrap_or(0);
            height_b.cmp(&height_a).then_with(|| score_b.cmp(&score_a))
        });

        peers
            .into_iter()
            .filter(|peer| peer.status == PeerStatus::Connected)
            .take(limit)
            .collect()
    }

    /// Update peer score
    pub fn update_score(&self, peer_id: &str, delta: i32) {
        let mut scores = self.scores.write();
        let score = scores.entry(peer_id.to_string()).or_insert(0);
        *score = (*score + delta).clamp(-1000, 1000);
        let current = *score;
        drop(scores);
        // Auto-ban persistently abusive peers.
        if current <= -100 {
            self.ban_peer(peer_id, 0);
        }
    }

    /// Ban peer
    pub fn ban_peer(&self, peer_id: &str, duration_secs: u64) {
        let now = current_timestamp();
        let duration = if duration_secs == 0 {
            self.default_ban_duration_secs
        } else {
            duration_secs
        };
        let expires_at = now.saturating_add(duration);

        self.banned_peers
            .write()
            .insert(peer_id.to_string(), expires_at);

        let peer_ip = self
            .peers
            .read()
            .get(peer_id)
            .map(|peer| peer.info.remote_addr.ip().to_string());
        if let Some(ip) = peer_ip {
            self.banned_ips.write().insert(ip, expires_at);
        }

        let _ = self.remove_peer(peer_id);
        self.persist_bans();

        tracing::info!("Banned peer {} for {} seconds", peer_id, duration);
    }

    /// Ban an IP and disconnect peers currently using that address.
    pub fn ban_ip(&self, ip: &str, duration_secs: u64) {
        let now = current_timestamp();
        let duration = if duration_secs == 0 {
            self.default_ban_duration_secs
        } else {
            duration_secs
        };
        let expires_at = now.saturating_add(duration);
        self.banned_ips.write().insert(ip.to_string(), expires_at);

        let affected = self
            .peers
            .read()
            .values()
            .filter(|peer| peer.info.remote_addr.ip().to_string() == ip)
            .map(|peer| peer.info.peer_id.clone())
            .collect::<Vec<_>>();
        for peer_id in affected {
            let _ = self.remove_peer(&peer_id);
            self.banned_peers.write().insert(peer_id, expires_at);
        }

        self.persist_bans();
        tracing::warn!("Banned ip {} for {} seconds", ip, duration);
    }

    /// Check if peer is banned
    pub fn is_banned(&self, peer_id: &str) -> bool {
        self.cleanup_expired_bans();
        self.banned_peers
            .read()
            .get(peer_id)
            .is_some_and(|expires| *expires > current_timestamp())
    }

    /// Check if remote IP is banned.
    pub fn is_ip_banned(&self, ip: &str) -> bool {
        self.cleanup_expired_bans();
        self.banned_ips
            .read()
            .get(ip)
            .is_some_and(|expires| *expires > current_timestamp())
    }

    /// Remove expired bans and persist updated state if needed.
    pub fn cleanup_expired_bans(&self) {
        let now = current_timestamp();
        let mut changed = false;
        {
            let mut peers = self.banned_peers.write();
            let old_len = peers.len();
            peers.retain(|_, expires_at| *expires_at > now);
            changed |= peers.len() != old_len;
        }
        {
            let mut ips = self.banned_ips.write();
            let old_len = ips.len();
            ips.retain(|_, expires_at| *expires_at > now);
            changed |= ips.len() != old_len;
        }

        if changed {
            self.persist_bans();
        }
    }

    /// Broadcast a message to all active peers except one source peer.
    /// Full outbound buffers drop the message and lower score (do not disconnect).
    pub fn broadcast_except(&self, source_peer_id: &str, message: ProtocolMessage) {
        // P2P-H4: collect targets under the read lock, then send after releasing
        // it — cloning + try_send must not happen while holding the peers lock.
        let targets: Vec<Arc<Peer>> = self
            .peers
            .read()
            .values()
            .filter(|peer| {
                peer.status == PeerStatus::Connected && peer.info.peer_id.as_str() != source_peer_id
            })
            .cloned()
            .collect();
        for peer in targets {
            if peer.send(message.clone()).is_err() {
                self.update_score(&peer.info.peer_id, -1);
            }
        }
    }

    /// Update last activity timestamp for a peer.
    pub fn touch_peer(&self, peer_id: &str) -> bool {
        let now = current_timestamp();
        let mut activity = self.activity.write();
        let Some(last_seen) = activity.get_mut(peer_id) else {
            return false;
        };
        let should_refresh_globals = now.saturating_sub(*last_seen) >= 1;
        *last_seen = now;
        drop(activity);
        // P2P-H3: only refresh the global peer snapshot at most once per second —
        // this method runs on every inbound frame, and the previous code cloned
        // the whole peer list per frame.
        if should_refresh_globals {
            set_global_peers(self.get_active_peer_infos());
        }
        true
    }

    /// Return whether a single peer has been idle for longer than `max_idle_secs`.
    /// Prefer over `stale_peers` when only one peer needs checking (P2P-M3).
    pub fn is_peer_stale(&self, peer_id: &str, max_idle_secs: u64) -> bool {
        let now = current_timestamp();
        self.activity
            .read()
            .get(peer_id)
            .is_some_and(|last_seen| now.saturating_sub(*last_seen) > max_idle_secs)
    }

    /// Return peer IDs that have been idle for longer than `max_idle_secs`.
    pub fn stale_peers(&self, max_idle_secs: u64) -> Vec<PeerId> {
        let now = current_timestamp();
        self.activity
            .read()
            .iter()
            .filter_map(|(peer_id, last_seen)| {
                if now.saturating_sub(*last_seen) > max_idle_secs {
                    Some(peer_id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Update peer advertised head height.
    pub fn update_peer_height(&self, peer_id: &str, height: u64) -> bool {
        let mut heights = self.heights.write();
        let Some(entry) = heights.get_mut(peer_id) else {
            return false;
        };
        *entry = height;
        // Keep global highest peer height up-to-date for out-of-sync checks.
        let max = heights.values().copied().max().unwrap_or(0);
        drop(heights);
        crate::set_global_highest_peer_height(max);
        true
    }

    /// Get highest announced head among connected peers.
    pub fn highest_peer_height(&self) -> u64 {
        self.heights.read().values().copied().max().unwrap_or(0)
    }

    fn persist_bans(&self) {
        let Some(path) = &self.banlist_path else {
            return;
        };

        let payload = PersistedBanList {
            peers: self.banned_peers.read().clone(),
            ips: self.banned_ips.read().clone(),
        };

        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                tracing::warn!(
                    "failed to create banlist directory {}: {}",
                    parent.display(),
                    err
                );
                return;
            }
        }

        let data = match serde_json::to_vec_pretty(&payload) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("failed to serialize p2p banlist: {}", err);
                return;
            }
        };

        if let Err(err) = fs::write(path, data) {
            tracing::warn!("failed to persist p2p banlist {}: {}", path.display(), err);
        }
    }
}

fn load_persisted_bans(
    path: Option<&PathBuf>,
) -> Option<(HashMap<PeerId, u64>, HashMap<String, u64>)> {
    let path = path?;
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("failed to read p2p banlist {}: {}", path.display(), err);
            }
            return None;
        }
    };
    let parsed = match serde_json::from_slice::<PersistedBanList>(&data) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!("failed to parse p2p banlist {}: {}", path.display(), err);
            return None;
        }
    };
    Some((parsed.peers, parsed.ips))
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
    fn test_peer_manager() {
        let manager = PeerManager::new(10);

        assert_eq!(manager.peer_count(), 0);
    }

    #[test]
    fn test_peer_info() {
        let mut info = PeerInfo::new(
            "test_peer".into(),
            "127.0.0.1:30303".parse().unwrap(),
            "127.0.0.1:8080".parse().unwrap(),
            10086,
        );

        assert_eq!(info.reputation, 100);

        info.increase_reputation(50);
        assert_eq!(info.reputation, 150);

        info.decrease_reputation(200);
        assert_eq!(info.reputation, -50);
    }

    #[test]
    fn test_banlist_persistence_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("p2p-banlist.json");

        let manager = PeerManager::new_with_policy(10, Some(path.clone()), 300);
        manager.ban_ip("127.0.0.8", 120);
        assert!(manager.is_ip_banned("127.0.0.8"));

        let reloaded = PeerManager::new_with_policy(10, Some(path), 300);
        assert!(reloaded.is_ip_banned("127.0.0.8"));
    }
}
