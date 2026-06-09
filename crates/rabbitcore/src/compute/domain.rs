//! Domain registry and domain config.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::primitives::DomainId;

/// Domain runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainConfig {
    /// Domain identifier.
    pub domain_id: DomainId,
    /// Human-readable domain name.
    pub name: String,
    /// VM kind used by the domain (for example wasm).
    pub vm: String,
    /// Whether domain accepts public transactions.
    pub public: bool,
}

/// Registry abstraction for domain metadata.
pub trait DomainRegistry: Send + Sync {
    /// Gets domain config by id.
    fn get_domain(&self, id: DomainId) -> Option<DomainConfig>;
    /// Upserts domain config.
    fn upsert_domain(&self, config: DomainConfig);
    /// Returns all known domains.
    fn list_domains(&self) -> Vec<DomainConfig>;
}

/// In-memory domain registry.
#[derive(Default)]
pub struct InMemoryDomainRegistry {
    configs: RwLock<HashMap<DomainId, DomainConfig>>,
}

impl InMemoryDomainRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }
}

impl DomainRegistry for InMemoryDomainRegistry {
    fn get_domain(&self, id: DomainId) -> Option<DomainConfig> {
        self.configs.read().get(&id).cloned()
    }

    fn upsert_domain(&self, config: DomainConfig) {
        self.configs.write().insert(config.domain_id, config);
    }

    fn list_domains(&self) -> Vec<DomainConfig> {
        self.configs.read().values().cloned().collect()
    }
}

impl<D: DomainRegistry + ?Sized> DomainRegistry for Arc<D> {
    fn get_domain(&self, id: DomainId) -> Option<DomainConfig> {
        self.as_ref().get_domain(id)
    }

    fn upsert_domain(&self, config: DomainConfig) {
        self.as_ref().upsert_domain(config)
    }

    fn list_domains(&self) -> Vec<DomainConfig> {
        self.as_ref().list_domains()
    }
}
