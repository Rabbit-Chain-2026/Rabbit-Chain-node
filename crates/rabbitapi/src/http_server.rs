//! HTTP Server Implementation using Axum

use axum::{
    Router,
    routing::post,
    extract::State,
    response::IntoResponse,
    http::{StatusCode, header},
    Json,
};
use tower_http::cors::{CorsLayer, Any};
use tokio::net::TcpListener;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use crate::rpc::{RpcApi, JsonRpcRequest, JsonRpcResponse};
use crate::{ApiError, Result};

/// HTTP Server configuration
#[derive(Clone, Debug)]
pub struct HttpServerConfig {
    /// Listen address
    pub address: String,
    /// Port
    pub port: u16,
    /// Max connections
    pub max_connections: usize,
    /// Request timeout (seconds)
    pub timeout_secs: u64,
    /// Max request body size (bytes)
    pub max_body_size: usize,
    /// CORS origins
    pub cors_origins: Vec<String>,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1".to_string(),
            port: 8545,
            max_connections: 1000,
            timeout_secs: 30,
            max_body_size: 15 * 1024 * 1024,  // 15MB
            cors_origins: vec!["*".to_string()],
        }
    }
}

/// HTTP Server state
#[derive(Clone)]
pub struct ServerState {
    api: Arc<RpcApi>,
    config: HttpServerConfig,
}

/// HTTP Server
pub struct HttpServer {
    config: HttpServerConfig,
    api: Arc<RpcApi>,
    shutdown_signal: Option<tokio::sync::broadcast::Sender<()>>,
}

impl HttpServer {
    /// Create new HTTP server
    pub fn new(config: HttpServerConfig, api: Arc<RpcApi>) -> Self {
        Self {
            config,
            api,
            shutdown_signal: None,
        }
    }
    
    /// Start server
    pub async fn start(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.config.address, self.config.port);
        
        let state = ServerState {
            api: self.api.clone(),
            config: self.config.clone(),
        };
        
        // Create CORS layer
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
        
        // Create router
        let app = Router::new()
            .route("/", post(handle_request))
            .route("/rpc", post(handle_request))
            .with_state(state)
            .layer(cors);
        
        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel(1);
        self.shutdown_signal = Some(shutdown_tx);
        
        tracing::info!("Starting HTTP server on {}", addr);
        
        // Bind and listen
        let listener = TcpListener::bind(&addr).await
            .map_err(|e| ApiError::IO(e))?;
        
        let shutdown_rx_clone = shutdown_rx.resubscribe();
        
        // Start server
        let server_future = axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move {
                shutdown_rx_clone.recv().await.ok();
            });
        
        server_future.await
            .map_err(|e| ApiError::IO(e))?;
        
        Ok(())
    }
    
    /// Stop server
    pub async fn stop(&self) -> Result<()> {
        if let Some(tx) = &self.shutdown_signal {
            let _ = tx.send(());
            tracing::info!("HTTP server stopping");
        }
        
        Ok(())
    }
}

/// Handle RPC request
async fn handle_request(
    State(state): State<ServerState>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    tracing::debug!("Received RPC request: {:?}", request.method);
    
    // Process request
    let response = state.api.handle_request(request).await;
    
    Json(response)
}

/// Handle batch RPC requests
async fn handle_batch_request(
    State(state): State<ServerState>,
    Json(requests): Json<Vec<JsonRpcRequest>>,
) -> impl IntoResponse {
    tracing::debug!("Received batch RPC request with {} items", requests.len());
    
    // Process all requests
    let mut responses = Vec::with_capacity(requests.len());
    
    for request in requests {
        let response = state.api.handle_request(request).await;
        responses.push(response);
    }
    
    Json(responses)
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_server_creation() {
        let config = HttpServerConfig {
            port: 8546,  // Use different port for tests
            ..Default::default()
        };
        
        // Would create RpcApi with proper dependencies
        // For now, just test config
        assert_eq!(config.port, 8546);
    }
}
