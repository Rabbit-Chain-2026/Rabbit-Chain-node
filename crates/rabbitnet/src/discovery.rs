//! Node discovery module backed by discovery v5 (Kademlia routing table).

use crate::{NetworkConfig, NetworkError, Result};
use discv5::{
    enr::{self, CombinedKey, NodeId},
    ConfigBuilder, Discv5, Enr, Event, ListenConfig,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration, MissedTickBehavior};
use rabbitcore::crypto::{keccak256, Hash};

const DISCOVERY_QUERY_INTERVAL_SECS: u64 = 12;

/// Node record
#[derive(Clone, Debug)]
pub struct NodeRecord {
    /// Peer ID
    pub peer_id: String,
    /// IP address
    pub ip: String,
    /// TCP port
    pub tcp_port: u16,
    /// UDP port
    pub udp_port: u16,
    /// Network ID announced by remote peer (0 if unknown)
    pub network_id: u64,
}

impl NodeRecord {
    pub fn from_enode(enode: &str) -> Result<Self> {
        // Parse enode://pubkey@ip:port
        if !enode.starts_with("enode://") {
            return Err(NetworkError::ProtocolError("Invalid enode format".into()));
        }

        let parts: Vec<&str> = enode[8..].split('@').collect();
        if parts.len() != 2 {
            return Err(NetworkError::ProtocolError("Invalid enode format".into()));
        }

        let peer_id = parts[0].to_string();
        let addr_parts: Vec<&str> = parts[1].split(':').collect();

        if addr_parts.len() != 2 {
            return Err(NetworkError::ProtocolError("Invalid address format".into()));
        }

        let ip = addr_parts[0].to_string();
        let port = addr_parts[1]
            .parse::<u16>()
            .map_err(|e| NetworkError::ProtocolError(format!("Invalid port: {e}")))?;

        Ok(Self {
            peer_id,
            ip,
            tcp_port: port,
            udp_port: port,
            network_id: 0,
        })
    }

    pub fn to_enode(&self) -> String {
        format!("enode://{}@{}:{}", self.peer_id, self.ip, self.tcp_port)
    }

    pub fn from_bootnode(raw: &str, network_id: u64) -> Result<Self> {
        if let Ok(node) = Self::from_enode(raw) {
            return Ok(node);
        }

        let enr = raw
            .parse::<Enr>()
            .map_err(|_| NetworkError::ProtocolError("Invalid bootnode format".into()))?;
        node_record_from_enr(&enr, network_id)
            .ok_or_else(|| NetworkError::ProtocolError("bootnode ENR missing address".into()))
    }
}

/// Kademlia bucket
#[derive(Clone, Debug)]
pub struct KBucket {
    nodes: Vec<NodeRecord>,
    last_updated: u64,
}

impl KBucket {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            last_updated: current_timestamp(),
        }
    }
}

/// Discovery service
pub struct Discovery {
    config: NetworkConfig,
    /// Local node ID
    node_id: String,
    /// Routing table (256 buckets)
    buckets: Arc<RwLock<Vec<KBucket>>>,
    /// Known nodes
    nodes: Arc<RwLock<HashMap<String, NodeRecord>>>,
    /// Background running flag
    running: Arc<AtomicBool>,
    /// Background task for discv5 event/query loop
    task: RwLock<Option<JoinHandle<()>>>,
    /// Base64 ENR for observability/debugging.
    local_enr: Arc<RwLock<Option<String>>>,
}

impl Discovery {
    pub fn new(config: &NetworkConfig) -> Result<Self> {
        // Generate node ID from key
        let node_id = generate_node_id();

        let buckets = (0..256).map(|_| KBucket::new()).collect();

        Ok(Self {
            config: config.clone(),
            node_id,
            buckets: Arc::new(RwLock::new(buckets)),
            nodes: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            task: RwLock::new(None),
            local_enr: Arc::new(RwLock::new(None)),
        })
    }

    pub fn local_enr_base64(&self) -> Option<String> {
        self.local_enr.read().clone()
    }

    pub async fn start(&self) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // Seed discovery table with statically configured bootnodes.
        for bootnode in &self.config.bootnodes {
            if let Ok(node) = NodeRecord::from_bootnode(bootnode, self.config.network_id) {
                let _ = self.add_node(node);
            }
        }

        let listen_ip = self
            .config
            .listen_addr
            .parse::<IpAddr>()
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        let listen_config = ListenConfig::from_ip(listen_ip, self.config.listen_port);

        let enr_key = CombinedKey::generate_secp256k1();
        let enr = build_local_enr(&self.config, &enr_key)?;
        *self.local_enr.write() = Some(enr.to_base64());

        let config = ConfigBuilder::new(listen_config).build();
        let mut discv5: Discv5 = Discv5::new(enr, enr_key, config)
            .map_err(|e| NetworkError::ConnectionError(format!("discv5 init failed: {e}")))?;

        for bootnode in &self.config.bootnodes {
            if let Some(enr) = parse_bootnode_as_enr(bootnode) {
                if let Err(err) = discv5.add_enr(enr) {
                    tracing::debug!(
                        "discovery add_enr failed for bootnode {}: {}",
                        bootnode,
                        err
                    );
                }
            }
        }

        discv5
            .start()
            .await
            .map_err(|e| NetworkError::ConnectionError(format!("discv5 start failed: {e}")))?;
        let mut events = discv5.event_stream().await.map_err(|e| {
            NetworkError::ConnectionError(format!("discv5 event stream failed: {e}"))
        })?;

        let running = self.running.clone();
        let buckets = self.buckets.clone();
        let nodes = self.nodes.clone();
        let node_id = self.node_id.clone();
        let network_id = self.config.network_id;

        let task = tokio::spawn(async move {
            tracing::info!("Starting discovery service via discv5");
            let mut query_tick = interval(Duration::from_secs(DISCOVERY_QUERY_INTERVAL_SECS));
            query_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            while running.load(Ordering::Relaxed) {
                tokio::select! {
                    _ = query_tick.tick() => {
                        if let Err(err) = discv5.find_node(NodeId::random()).await {
                            tracing::debug!("discovery find_node failed: {}", err);
                        }
                    }
                    ev = events.recv() => {
                        match ev {
                            Some(Event::Discovered(enr)) => {
                                if let Some(node) = node_record_from_enr(&enr, network_id) {
                                    let _ = insert_node_record(&node_id, &buckets, &nodes, node);
                                }
                            }
                            Some(Event::EnrAdded { enr, .. }) => {
                                if let Some(node) = node_record_from_enr(&enr, network_id) {
                                    let _ = insert_node_record(&node_id, &buckets, &nodes, node);
                                }
                            }
                            Some(Event::SessionEstablished(enr, _)) => {
                                if let Some(node) = node_record_from_enr(&enr, network_id) {
                                    let _ = insert_node_record(&node_id, &buckets, &nodes, node);
                                }
                            }
                            Some(Event::SocketUpdated(addr)) => {
                                tracing::debug!("discovery socket updated: {}", addr);
                            }
                            Some(Event::NodeInserted { .. }) | Some(Event::TalkRequest(_)) => {}
                            None => {
                                tracing::warn!("discovery event stream closed");
                                break;
                            }
                        }
                    }
                }
            }
        });

        *self.task.write() = Some(task);
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.task.write().take() {
            task.abort();
        }
        tracing::info!("Stopping discovery service");
        Ok(())
    }

    /// Add node to routing table
    pub fn add_node(&self, node: NodeRecord) -> bool {
        insert_node_record(&self.node_id, &self.buckets, &self.nodes, node)
    }

    /// Get closest nodes to target
    pub fn get_closest_nodes(&self, target: &str, limit: usize) -> Vec<NodeRecord> {
        let distance = calculate_distance(&self.node_id, target);
        let bucket_index = 255 - distance.leading_zeros() as usize;

        let buckets = self.buckets.read();
        let mut nodes = Vec::new();

        // Collect from closest buckets
        for i in 0..256 {
            let idx = bucket_index.abs_diff(i);

            if idx < buckets.len() {
                for node in &buckets[idx].nodes {
                    nodes.push(node.clone());
                    if nodes.len() >= limit {
                        return nodes;
                    }
                }
            }
        }

        nodes
    }

    /// Get random nodes
    pub fn get_random_nodes(&self, limit: usize) -> Vec<NodeRecord> {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let nodes = self.nodes.read();
        let mut selected = Vec::new();

        for node in nodes.values() {
            if selected.len() >= limit {
                break;
            }

            if rng.gen_bool(0.5) {
                selected.push(node.clone());
            }
        }

        selected
    }

    /// Remove node
    pub fn remove_node(&self, peer_id: &str) {
        self.nodes.write().remove(peer_id);

        // Would also remove from bucket
    }
}

fn build_local_enr(config: &NetworkConfig, enr_key: &CombinedKey) -> Result<Enr> {
    let mut builder = enr::Enr::builder();

    let advertised_ip = config
        .external_addr
        .as_ref()
        .and_then(|raw| parse_maybe_socket_ip(raw))
        .or_else(|| parse_maybe_socket_ip(&config.listen_addr));

    if let Some(ip) = advertised_ip {
        match ip {
            IpAddr::V4(ip4) => {
                builder.ip4(ip4);
                builder.tcp4(config.listen_port);
                builder.udp4(config.listen_port);
            }
            IpAddr::V6(ip6) => {
                builder.ip6(ip6);
                builder.tcp6(config.listen_port);
                builder.udp6(config.listen_port);
            }
        }
    } else {
        // Keep port fields so peers can still connect when IP gets auto-updated.
        builder.udp4(config.listen_port);
        builder.tcp4(config.listen_port);
    }

    builder.build(enr_key).map_err(|err| {
        NetworkError::ConnectionError(format!("local ENR construction failed: {err}"))
    })
}

fn parse_maybe_socket_ip(raw: &str) -> Option<IpAddr> {
    raw.parse::<SocketAddr>()
        .map(|addr| addr.ip())
        .or_else(|_| raw.parse::<IpAddr>())
        .ok()
        .filter(|ip| !ip.is_unspecified())
}

fn parse_bootnode_as_enr(raw: &str) -> Option<Enr> {
    raw.parse::<Enr>().ok()
}

fn node_record_from_enr(enr: &Enr, network_id: u64) -> Option<NodeRecord> {
    if let Some(ip4) = enr.ip4() {
        let tcp_port = enr.tcp4().or_else(|| enr.udp4())?;
        let udp_port = enr.udp4().unwrap_or(tcp_port);
        return Some(NodeRecord {
            peer_id: enr.node_id().to_string(),
            ip: ip4.to_string(),
            tcp_port,
            udp_port,
            network_id,
        });
    }

    if let Some(ip6) = enr.ip6() {
        let tcp_port = enr.tcp6().or_else(|| enr.udp6())?;
        let udp_port = enr.udp6().unwrap_or(tcp_port);
        return Some(NodeRecord {
            peer_id: enr.node_id().to_string(),
            ip: ip6.to_string(),
            tcp_port,
            udp_port,
            network_id,
        });
    }

    None
}

fn insert_node_record(
    local_node_id: &str,
    buckets: &RwLock<Vec<KBucket>>,
    nodes: &RwLock<HashMap<String, NodeRecord>>,
    node: NodeRecord,
) -> bool {
    // Calculate distance and bucket index
    let distance = calculate_distance(local_node_id, &node.peer_id);
    let bucket_index = 255 - distance.leading_zeros() as usize;

    let mut buckets = buckets.write();
    let bucket = &mut buckets[bucket_index];

    // Check if already exists
    if bucket.nodes.iter().any(|n| n.peer_id == node.peer_id) {
        bucket.last_updated = current_timestamp();
        nodes.write().insert(node.peer_id.clone(), node);
        return false;
    }

    // Add if bucket not full
    if bucket.nodes.len() < 20 {
        bucket.nodes.push(node.clone());
        bucket.last_updated = current_timestamp();
        nodes.write().insert(node.peer_id.clone(), node);
        true
    } else {
        false
    }
}

fn generate_node_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();

    let mut bytes = [0u8; 64];
    for byte in &mut bytes {
        *byte = rng.gen();
    }

    hex::encode(bytes)
}

fn calculate_distance(id1: &str, id2: &str) -> Hash {
    // XOR distance
    let bytes1 = normalize_node_id_bytes(id1);
    let bytes2 = normalize_node_id_bytes(id2);

    let mut distance = [0u8; 32];
    for (i, slot) in distance.iter_mut().enumerate() {
        *slot = bytes1[i] ^ bytes2[i];
    }

    Hash::from_bytes(distance)
}

fn normalize_node_id_bytes(id: &str) -> [u8; 32] {
    let normalized = id.trim().strip_prefix("0x").unwrap_or(id.trim());
    if let Ok(decoded) = hex::decode(normalized) {
        if decoded.len() >= 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&decoded[..32]);
            return out;
        }
    }
    keccak256(normalized.as_bytes())
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
    fn test_node_record_from_enode_roundtrip() {
        let record = NodeRecord::from_enode("enode://peer123@127.0.0.1:30303").unwrap();
        assert_eq!(record.peer_id, "peer123");
        assert_eq!(record.ip, "127.0.0.1");
        assert_eq!(record.tcp_port, 30303);
        assert_eq!(record.to_enode(), "enode://peer123@127.0.0.1:30303");
    }

    #[test]
    fn test_node_record_from_enode_rejects_invalid_port() {
        let err = NodeRecord::from_enode("enode://peer123@127.0.0.1:not-a-port")
            .expect_err("invalid port should fail");
        assert!(matches!(err, NetworkError::ProtocolError(_)));
    }

    #[test]
    fn test_calculate_distance_supports_non_hex_peer_ids() {
        let same = calculate_distance("peer-A", "peer-A");
        assert_eq!(same, Hash::zero());

        let different = calculate_distance("peer-A", "peer-B");
        assert_ne!(different, Hash::zero());
    }

    #[test]
    fn test_extract_node_record_from_enr() {
        let key = CombinedKey::generate_secp256k1();
        let enr = {
            let mut builder = enr::Enr::builder();
            builder.ip4(std::net::Ipv4Addr::LOCALHOST);
            builder.udp4(19000);
            builder.tcp4(19001);
            builder.build(&key).unwrap()
        };

        let node = node_record_from_enr(&enr, 10086).expect("enr should convert");
        assert_eq!(node.ip, "127.0.0.1");
        assert_eq!(node.udp_port, 19000);
        assert_eq!(node.tcp_port, 19001);
        assert_eq!(node.network_id, 10086);
    }

    #[test]
    fn test_bootnode_enr_support() {
        let key = CombinedKey::generate_secp256k1();
        let enr = {
            let mut builder = enr::Enr::builder();
            builder.ip4(std::net::Ipv4Addr::LOCALHOST);
            builder.udp4(20000);
            builder.tcp4(20001);
            builder.build(&key).unwrap()
        };
        let node = NodeRecord::from_bootnode(&enr.to_base64(), 2026).unwrap();
        assert_eq!(node.ip, "127.0.0.1");
        assert_eq!(node.udp_port, 20000);
        assert_eq!(node.tcp_port, 20001);
        assert_eq!(node.network_id, 2026);
    }
}
