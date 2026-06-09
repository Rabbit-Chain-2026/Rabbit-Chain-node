//! WebSocket Server Implementation
//!
//! Provides real-time subscriptions for:
//! - New block headers
//! - New pending operations
//! - Event logs

mod server;
mod subscription;

pub use server::*;
pub use subscription::*;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

/// WebSocket error types
#[derive(Error, Debug, Clone)]
pub enum WsError {
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("Subscription error: {0}")]
    Subscription(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Channel error")]
    Channel,
}

pub type Result<T> = std::result::Result<T, WsError>;

/// WebSocket configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsConfig {
    /// Listen address
    pub address: String,
    /// Port
    pub port: u16,
    /// Max connections
    pub max_connections: usize,
    /// Max subscriptions per connection
    pub max_subscriptions: usize,
    /// Ping interval (seconds)
    pub ping_interval: u64,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1".to_string(),
            port: 8546,
            max_connections: 100,
            max_subscriptions: 10,
            ping_interval: 30,
        }
    }
}

/// Subscription types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubscriptionType {
    /// New block headers
    NewHeads,
    /// New pending operations
    PendingOperations,
    /// Event logs
    Logs(LogsFilter),
    /// Syncing status
    Syncing,
}

/// Logs filter
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsFilter {
    /// From block
    pub from_block: Option<String>,
    /// To block
    pub to_block: Option<String>,
    /// Contract addresses
    pub address: Option<Vec<String>>,
    /// Topics
    pub topics: Vec<Option<String>>,
}

/// Subscription message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscriptionMessage {
    /// Subscription ID
    pub subscription: String,
    /// Result data
    pub result: serde_json::Value,
}

/// WebSocket notification
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: SubscriptionMessage,
}

impl WsNotification {
    pub fn new(subscription: String, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: "rabbit_subscription".to_string(),
            params: SubscriptionMessage {
                subscription,
                result,
            },
        }
    }
}

/// Broadcast channels for different subscription types
pub struct BroadcastChannels {
    /// New block headers
    pub new_heads: broadcast::Sender<serde_json::Value>,
    /// New pending transactions
    pub new_pending_txs: broadcast::Sender<serde_json::Value>,
    /// Logs
    pub logs: broadcast::Sender<(LogsFilter, serde_json::Value)>,
    /// Syncing status
    pub syncing: broadcast::Sender<serde_json::Value>,
}

impl BroadcastChannels {
    pub fn new() -> Self {
        let (new_heads_tx, _) = broadcast::channel(100);
        let (new_pending_txs_tx, _) = broadcast::channel(1000);
        let (logs_tx, _) = broadcast::channel(100);
        let (syncing_tx, _) = broadcast::channel(10);

        Self {
            new_heads: new_heads_tx,
            new_pending_txs: new_pending_txs_tx,
            logs: logs_tx,
            syncing: syncing_tx,
        }
    }
}

impl Default for BroadcastChannels {
    fn default() -> Self {
        Self::new()
    }
}

/// Subscription manager
pub struct SubscriptionManager {
    /// Active subscriptions
    subscriptions: RwLock<std::collections::HashMap<String, SubscriptionType>>,
    /// Broadcast channels
    channels: Arc<BroadcastChannels>,
    /// Connection count
    connection_count: RwLock<usize>,
    /// Max connections
    max_connections: usize,
}

impl SubscriptionManager {
    pub fn new(max_connections: usize) -> Self {
        Self {
            subscriptions: RwLock::new(std::collections::HashMap::new()),
            channels: Arc::new(BroadcastChannels::new()),
            connection_count: RwLock::new(0),
            max_connections,
        }
    }

    /// Create new subscription
    pub fn create_subscription(&self, sub_type: SubscriptionType) -> String {
        use uuid::Uuid;
        let id = Uuid::new_v4().to_string();

        self.subscriptions.write().insert(id.clone(), sub_type);

        id
    }

    /// Remove subscription
    pub fn remove_subscription(&self, id: &str) -> bool {
        self.subscriptions.write().remove(id).is_some()
    }

    /// Get subscription type
    pub fn get_subscription(&self, id: &str) -> Option<SubscriptionType> {
        self.subscriptions.read().get(id).cloned()
    }

    /// Get all subscriptions
    pub fn get_all_subscriptions(&self) -> Vec<(String, SubscriptionType)> {
        self.subscriptions
            .read()
            .iter()
            .map(|(id, ty)| (id.clone(), ty.clone()))
            .collect()
    }

    /// Get broadcast channel
    pub fn get_channels(&self) -> Arc<BroadcastChannels> {
        self.channels.clone()
    }

    /// Increment connection count
    pub fn add_connection(&self) -> bool {
        let mut count = self.connection_count.write();

        if *count >= self.max_connections {
            return false;
        }

        *count += 1;
        true
    }

    /// Decrement connection count
    pub fn remove_connection(&self) {
        let mut count = self.connection_count.write();
        *count = count.saturating_sub(1);
    }

    /// Get connection count
    pub fn connection_count(&self) -> usize {
        *self.connection_count.read()
    }

    /// Broadcast new block header
    pub fn broadcast_new_head(&self, header: serde_json::Value) -> Result<()> {
        self.channels
            .new_heads
            .send(header)
            .map_err(|_| WsError::Channel)?;
        Ok(())
    }

    /// Broadcast new pending operation
    pub fn broadcast_new_pending_operation(&self, tx_hash: serde_json::Value) -> Result<()> {
        self.channels
            .new_pending_txs
            .send(tx_hash)
            .map_err(|_| WsError::Channel)?;
        Ok(())
    }

    /// Broadcast log
    pub fn broadcast_log(&self, filter: &LogsFilter, log: serde_json::Value) -> Result<()> {
        self.channels
            .logs
            .send((filter.clone(), log))
            .map_err(|_| WsError::Channel)?;
        Ok(())
    }

    /// Broadcast syncing status
    pub fn broadcast_syncing(&self, status: serde_json::Value) -> Result<()> {
        self.channels
            .syncing
            .send(status)
            .map_err(|_| WsError::Channel)?;
        Ok(())
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new(100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_manager() {
        let manager = SubscriptionManager::new(10);

        // Create subscription
        let id = manager.create_subscription(SubscriptionType::NewHeads);
        assert!(!id.is_empty());

        // Get subscription
        let sub = manager.get_subscription(&id);
        assert_eq!(sub, Some(SubscriptionType::NewHeads));

        // Remove subscription
        assert!(manager.remove_subscription(&id));
        assert!(!manager.remove_subscription(&id));
    }

    #[test]
    fn test_connection_limit() {
        let manager = SubscriptionManager::new(2);

        assert!(manager.add_connection());
        assert!(manager.add_connection());
        assert!(!manager.add_connection()); // Should fail

        manager.remove_connection();
        assert!(manager.add_connection()); // Should succeed now
    }
}
