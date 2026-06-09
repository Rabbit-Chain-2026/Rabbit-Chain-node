//! TCP Transport Layer for P2P Communication

use crate::{NetworkError, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use bytes::{BytesMut, Bytes};
use std::net::SocketAddr;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::{info, warn, error};

/// TCP Transport configuration
#[derive(Clone, Debug)]
pub struct TcpConfig {
    /// Listen address
    pub listen_addr: String,
    /// Listen port
    pub listen_port: u16,
    /// Max connections
    pub max_connections: usize,
    /// Read timeout (seconds)
    pub read_timeout: u64,
    /// Write timeout (seconds)
    pub write_timeout: u64,
    /// Max packet size
    pub max_packet_size: usize,
    /// Enable encryption
    pub enable_encryption: bool,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0".to_string(),
            listen_port: 30303,
            max_connections: 100,
            read_timeout: 30,
            write_timeout: 30,
            max_packet_size: 10 * 1024 * 1024,  // 10MB
            enable_encryption: true,
        }
    }
}

/// TCP Connection
pub struct TcpConnection {
    /// Remote address
    pub remote_addr: SocketAddr,
    /// Local address
    pub local_addr: SocketAddr,
    /// TCP stream
    stream: TcpStream,
    /// Read buffer
    read_buffer: BytesMut,
    /// Is active
    is_active: bool,
}

impl TcpConnection {
    /// Create new connection from stream
    pub async fn new(stream: TcpStream) -> Result<Self> {
        let remote_addr = stream.peer_addr()?;
        let local_addr = stream.local_addr()?;
        
        // Configure TCP
        stream.set_nodelay(true)?;
        
        Ok(Self {
            remote_addr,
            local_addr,
            stream,
            read_buffer: BytesMut::with_capacity(4096),
            is_active: true,
        })
    }
    
    /// Read data from connection
    pub async fn read(&mut self) -> Result<Bytes> {
        // Read packet header (4 bytes)
        let mut header = [0u8; 4];
        
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.stream.read_exact(&mut header)
        ).await
            .map_err(|_| NetworkError::ConnectionError("Read timeout".into()))?
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        // Parse packet size (RLP encoded)
        let packet_size = u32::from_be_bytes(header) as usize;
        
        if packet_size > 10 * 1024 * 1024 {
            return Err(NetworkError::ConnectionError("Packet too large".into()));
        }
        
        // Read packet data
        let mut data = vec![0u8; packet_size];
        
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.stream.read_exact(&mut data)
        ).await
            .map_err(|_| NetworkError::ConnectionError("Read timeout".into()))?
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        Ok(Bytes::from(data))
    }
    
    /// Write data to connection
    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        // Write packet header (4 bytes)
        let header = (data.len() as u32).to_be_bytes();
        
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.stream.write_all(&header)
        ).await
            .map_err(|_| NetworkError::ConnectionError("Write timeout".into()))?
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        // Write packet data
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.stream.write_all(data)
        ).await
            .map_err(|_| NetworkError::ConnectionError("Write timeout".into()))?
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        self.stream.flush().await
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        Ok(())
    }
    
    /// Close connection
    pub async fn close(&mut self) -> Result<()> {
        self.is_active = false;
        
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.stream.shutdown()
        ).await
            .ok();
        
        Ok(())
    }
    
    /// Check if connection is active
    pub fn is_active(&self) -> bool {
        self.is_active
    }
    
    /// Get remote address
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }
}

/// TCP Transport manager
pub struct TcpTransport {
    /// Configuration
    config: TcpConfig,
    /// TCP listener
    listener: Option<TcpListener>,
    /// Active connections
    connections: RwLock<Vec<Arc<RwLock<TcpConnection>>>>,
    /// Connection count
    connection_count: RwLock<usize>,
    /// Is running
    is_running: bool,
}

impl TcpTransport {
    /// Create new TCP transport
    pub fn new(config: TcpConfig) -> Self {
        Self {
            config,
            listener: None,
            connections: RwLock::new(Vec::new()),
            connection_count: RwLock::new(0),
            is_running: false,
        }
    }
    
    /// Start listening
    pub async fn start(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.config.listen_addr, self.config.listen_port);
        
        info!("Starting TCP listener on {}", addr);
        
        let listener = TcpListener::bind(&addr).await
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        self.listener = Some(listener);
        self.is_running = true;
        
        info!("TCP listener started");
        
        Ok(())
    }
    
    /// Stop listening
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping TCP listener");
        
        // Close all connections
        let mut connections = self.connections.write();
        for conn in connections.iter() {
            let mut conn = conn.write();
            let _ = conn.close().await;
        }
        connections.clear();
        
        *self.connection_count.write() = 0;
        self.is_running = false;
        
        info!("TCP listener stopped");
        
        Ok(())
    }
    
    /// Accept new connections
    pub async fn accept_connections(&self) -> Result<()> {
        let listener = self.listener.as_ref()
            .ok_or_else(|| NetworkError::ConnectionError("Not listening".into()))?;
        
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("New connection from {}", addr);
                    
                    // Check connection limit
                    let count = *self.connection_count.read();
                    if count >= self.config.max_connections {
                        warn!("Connection limit reached, rejecting {}", addr);
                        continue;
                    }
                    
                    // Create connection
                    match TcpConnection::new(stream).await {
                        Ok(conn) => {
                            let conn = Arc::new(RwLock::new(conn));
                            self.connections.write().push(conn.clone());
                            *self.connection_count.write() += 1;
                            
                            // Handle connection in background
                            tokio::spawn(handle_connection(conn.clone()));
                        }
                        Err(e) => {
                            error!("Failed to create connection: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Accept error: {}", e);
                }
            }
        }
    }
    
    /// Connect to remote peer
    pub async fn connect(&self, addr: SocketAddr) -> Result<Arc<RwLock<TcpConnection>>> {
        info!("Connecting to {}", addr);
        
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            TcpStream::connect(addr)
        ).await
            .map_err(|_| NetworkError::ConnectionError("Connection timeout".into()))?
            .map_err(|e| NetworkError::ConnectionError(e.to_string()))?;
        
        let conn = TcpConnection::new(stream).await?;
        let conn = Arc::new(RwLock::new(conn));
        
        self.connections.write().push(conn.clone());
        *self.connection_count.write() += 1;
        
        info!("Connected to {}", addr);
        
        Ok(conn)
    }
    
    /// Get active connection count
    pub fn connection_count(&self) -> usize {
        *self.connection_count.read()
    }
    
    /// Get all connections
    pub fn get_connections(&self) -> Vec<Arc<RwLock<TcpConnection>>> {
        self.connections.read().clone()
    }
    
    /// Remove connection
    pub fn remove_connection(&self, addr: &SocketAddr) {
        let mut connections = self.connections.write();
        connections.retain(|conn| {
            let conn = conn.read();
            if &conn.remote_addr == addr {
                *self.connection_count.write() -= 1;
                false
            } else {
                true
            }
        });
    }
    
    /// Check if running
    pub fn is_running(&self) -> bool {
        self.is_running
    }
}

/// Handle individual connection
async fn handle_connection(conn: Arc<RwLock<TcpConnection>>) {
    let remote_addr = conn.read().remote_addr;
    
    loop {
        // Read message
        let mut conn_lock = conn.write();
        
        if !conn_lock.is_active() {
            break;
        }
        
        match conn_lock.read().await {
            Ok(data) => {
                // Process message
                // Would pass to protocol layer
                info!("Received {} bytes from {}", data.len(), remote_addr);
            }
            Err(e) => {
                warn!("Read error from {}: {}", remote_addr, e);
                break;
            }
        }
        
        drop(conn_lock);
        
        // Small delay to prevent tight loop
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    
    // Cleanup
    conn.write().is_active = false;
    info!("Connection closed: {}", remote_addr);
}

/// RLPx frame (simplified)
pub struct RLPxFrame {
    /// Frame type
    pub frame_type: u8,
    /// Frame data
    pub data: Bytes,
}

impl RLPxFrame {
    /// Encode frame
    pub fn encode(&self) -> Bytes {
        let mut data = BytesMut::with_capacity(1 + self.data.len());
        data.put_u8(self.frame_type);
        data.extend_from_slice(&self.data);
        data.freeze()
    }
    
    /// Decode frame
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(NetworkError::ProtocolError("Empty frame".into()));
        }
        
        let frame_type = data[0];
        let frame_data = Bytes::copy_from_slice(&data[1..]);
        
        Ok(Self {
            frame_type,
            data: frame_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_tcp_transport() {
        let config = TcpConfig {
            listen_port: 30304,  // Use different port for tests
            max_connections: 10,
            ..Default::default()
        };
        
        let mut transport = TcpTransport::new(config);
        
        // Start listener
        transport.start().await.unwrap();
        assert!(transport.is_running());
        
        // Stop listener
        transport.stop().await.unwrap();
        assert!(!transport.is_running());
    }
    
    #[test]
    fn test_rlpx_frame() {
        let frame = RLPxFrame {
            frame_type: 1,
            data: Bytes::from(vec![1, 2, 3, 4]),
        };
        
        let encoded = frame.encode();
        assert_eq!(encoded.len(), 5);
        assert_eq!(encoded[0], 1);
        
        let decoded = RLPxFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.frame_type, 1);
        assert_eq!(decoded.data.as_ref(), &[1, 2, 3, 4]);
    }
}
