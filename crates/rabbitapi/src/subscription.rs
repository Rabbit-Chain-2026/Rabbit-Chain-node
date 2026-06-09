//! Subscription module - Placeholder

use crate::ws::SubscriptionManager;

pub use crate::ws::SubscriptionManager as SubscriptionHandler;

/// Create subscription manager
pub fn create_subscription_manager(max_connections: usize) -> SubscriptionManager {
    SubscriptionManager::new(max_connections)
}
