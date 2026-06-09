//! REST API module - Placeholder

use axum::{extract::State, response::Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// REST server configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestConfig {
    pub address: String,
    pub port: u16,
}

impl Default for RestConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0".to_string(),
            port: 8080,
        }
    }
}

/// REST server
pub struct RestServer {
    config: RestConfig,
}

impl RestServer {
    pub fn new(config: RestConfig) -> Self {
        Self { config }
    }

    pub async fn start(&self) -> crate::Result<()> {
        // Placeholder implementation
        tracing::info!(
            "REST API server would start on {}:{}",
            self.config.address,
            self.config.port
        );
        Ok(())
    }

    pub async fn stop(&self) -> crate::Result<()> {
        Ok(())
    }
}

/// Health check response
#[derive(Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Create REST router
pub fn create_router() -> Router<Arc<()>> {
    Router::new().route("/health", axum::routing::get(health_check))
}

/// Health check endpoint
async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
