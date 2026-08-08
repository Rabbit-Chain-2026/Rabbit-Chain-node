//! RabbitChain API Services
//!
//! Provides:
//! - JSON-RPC server (native method surface)
//! - REST API
//! - WebSocket subscriptions

#![allow(missing_docs)]
#![allow(rustdoc::missing_crate_level_docs)]
#![allow(unused)]

pub mod rest;
pub mod rpc;
pub mod subscription;
pub mod ws;

pub use rest::{RestConfig, RestServer};
pub use rpc::{wire_reorg_notifications, RpcApi, RpcConfig, RpcServer};
pub use ws::{SubscriptionManager, WsConfig, WsServer};

use std::sync::Arc;
use thiserror::Error;
#[cfg(test)]
use std::time::Duration;

/// API error types
#[derive(Error, Debug)]
pub enum ApiError {
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Method not found: {0}")]
    MethodNotFound(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// API configuration
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ApiConfig {
    /// HTTP RPC config
    pub http_rpc: RpcConfig,
    /// WebSocket config
    pub ws: WsConfig,
    /// REST API config
    pub rest: RestConfig,
    /// Enable HTTP RPC
    pub enable_http_rpc: bool,
    /// Enable WebSocket
    pub enable_ws: bool,
    /// Enable REST API
    pub enable_rest: bool,
    /// CORS origins
    pub cors_origins: Vec<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            http_rpc: RpcConfig::default(),
            ws: WsConfig::default(),
            rest: RestConfig::default(),
            enable_http_rpc: true,
            enable_ws: true,
            enable_rest: true,
            cors_origins: vec!["*".to_string()],
        }
    }
}

/// API service
pub struct ApiService {
    config: ApiConfig,
    rpc_server: Option<RpcServer>,
    ws_server: Option<WsServer>,
    rest_server: Option<RestServer>,
}

impl ApiService {
    /// Create new API service.
    ///
    /// Returns an error when configuration is invalid.
    pub fn try_new(config: ApiConfig) -> Result<Self> {
        let rpc_server = if config.enable_http_rpc {
            Some(
                RpcServer::try_new(config.http_rpc.clone())
                    .map_err(|e| ApiError::Rpc(e.to_string()))?,
            )
        } else {
            None
        };

        let ws_server = if config.enable_ws {
            Some(WsServer::new(config.ws.clone()))
        } else {
            None
        };

        let rest_server = if config.enable_rest {
            Some(RestServer::new(config.rest.clone()))
        } else {
            None
        };

        Ok(Self {
            config,
            rpc_server,
            ws_server,
            rest_server,
        })
    }

    /// Create new API service with validation.
    pub fn new(config: ApiConfig) -> Result<Self> {
        Self::try_new(config)
    }

    /// Start API service
    pub async fn start(&self) -> Result<()> {
        if let Some(rpc) = &self.rpc_server {
            rpc.start().await?;
            tracing::info!("HTTP RPC started on {}", self.config.http_rpc.address);
        }

        if let Some(ws) = &self.ws_server {
            ws.start().await?;
            tracing::info!("WebSocket started on {}", self.config.ws.address);
        }

        if let Some(rest) = &self.rest_server {
            rest.start().await?;
            tracing::info!("REST API started on {}", self.config.rest.address);
        }

        Ok(())
    }

    /// Stop API service
    pub async fn stop(&self) -> Result<()> {
        if let Some(rpc) = &self.rpc_server {
            rpc.stop().await?;
        }

        if let Some(ws) = &self.ws_server {
            ws.stop().await?;
        }

        if let Some(rest) = &self.rest_server {
            rest.stop().await?;
        }

        Ok(())
    }

    /// Underlying JSON-RPC API (for wiring network reorg notifications).
    pub fn rpc_api(&self) -> Option<Arc<RpcApi>> {
        self.rpc_server.as_ref().and_then(|rpc| rpc.api())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::ComputeBackend;

    #[tokio::test]
    async fn api_service_starts_with_mem_compute_backend() {
        let mut config = RpcConfig::default();
        config.address = "127.0.0.1".to_string();
        config.port = 0;
        config.compute_backend = ComputeBackend::Mem;
        config.compute_db_path = ":memory:".to_string();
        config.mining_enabled = false;

        let mut api_cfg = ApiConfig::default();
        api_cfg.http_rpc = config;
        api_cfg.ws.port = 0;
        api_cfg.rest.port = 0;

        let service = ApiService::try_new(api_cfg).expect("api service should build");
        service.start().await.expect("api service should start");
        service.stop().await.expect("api service should stop");
    }
}
