//! TCP transport layer for distribution.
//!
//! Provides TCP-based connections for Rust-to-Rust distribution
//! using our native protocol (postcard-encoded DistMessages).
//!
//! For BEAM/Erlang interoperability, see the `erlang` module which
//! uses the full Erlang Distribution Protocol via `edp_client`.

use super::protocol::{DistError, DistMessage, frame_message, parse_frame};
use super::traits::{self, Transport, TransportConnection as TransportConnectionTrait};
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// TCP transport for Rust-to-Rust distribution.
///
/// This is a simpler alternative to QUIC for environments where
/// QUIC is not available or not preferred. It uses our native
/// postcard-based protocol over raw TCP connections.
///
/// Note: This does NOT provide Erlang/BEAM compatibility.
/// For BEAM interop, use the `erlang` distribution module.
pub struct TcpTransport {
    /// The TCP listener for incoming connections.
    listener: TcpListener,
    /// Our node name for handshakes.
    node_name: String,
    /// Our creation number.
    creation: u32,
}

impl TcpTransport {
    /// Create a new TCP transport bound to the given address.
    pub async fn bind(
        addr: SocketAddr,
        node_name: String,
        creation: u32,
    ) -> Result<Self, DistError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| DistError::Io(e.to_string()))?;

        tracing::info!(%addr, "TCP transport listening");

        Ok(Self {
            listener,
            node_name,
            creation,
        })
    }

    /// Accept an incoming connection.
    pub async fn accept(&self) -> Option<TcpConnection> {
        match self.listener.accept().await {
            Ok((stream, remote_addr)) => {
                tracing::debug!(%remote_addr, "Accepted incoming TCP connection");
                Some(TcpConnection::new(stream, remote_addr))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to accept TCP connection");
                None
            }
        }
    }

    /// Connect to a remote node.
    pub async fn connect(
        &self,
        addr: SocketAddr,
        _server_name: &str,
    ) -> Result<TcpConnection, DistError> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| DistError::Connect(e.to_string()))?;

        tracing::debug!(%addr, "Connected to remote node via TCP");

        Ok(TcpConnection::new(stream, addr))
    }

    /// Get the local address this transport is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, DistError> {
        self.listener
            .local_addr()
            .map_err(|e| DistError::Io(e.to_string()))
    }

    /// Get our node name.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Get our creation number.
    pub fn creation(&self) -> u32 {
        self.creation
    }

    /// Close the transport (drop the listener).
    pub fn close(&self) {
        // TcpListener doesn't have an explicit close - dropping it closes.
        // This method exists for API symmetry with QuicTransport.
    }
}

/// A TCP connection to a remote node.
///
/// Wraps a TcpStream with framing and message serialization.
#[derive(Clone)]
pub struct TcpConnection {
    /// The underlying TCP stream, wrapped in Arc<Mutex> for cloning.
    stream: Arc<Mutex<TcpStream>>,
    /// Remote peer address.
    remote_addr: SocketAddr,
    /// Buffer for incomplete messages.
    read_buffer: Arc<Mutex<Vec<u8>>>,
}

impl TcpConnection {
    /// Create a new connection wrapper.
    pub fn new(stream: TcpStream, remote_addr: SocketAddr) -> Self {
        Self {
            stream: Arc::new(Mutex::new(stream)),
            remote_addr,
            read_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Send a message on this connection.
    pub async fn send_message(&self, msg: &DistMessage) -> Result<(), DistError> {
        let frame = frame_message(msg)?;
        let mut stream = self.stream.lock().await;
        stream
            .write_all(&frame)
            .await
            .map_err(|e| DistError::Io(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| DistError::Io(e.to_string()))?;
        Ok(())
    }

    /// Receive a message from this connection.
    pub async fn recv_message(&self) -> Result<DistMessage, DistError> {
        let mut buf = self.read_buffer.lock().await;
        let mut stream = self.stream.lock().await;

        loop {
            // Try to parse a complete message from buffer
            if let Some((msg, consumed)) = parse_frame(&buf)? {
                buf.drain(..consumed);
                return Ok(msg);
            }

            // Need more data
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) => {
                    // Connection closed
                    return Err(DistError::ConnectionClosed);
                }
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(e) => {
                    return Err(DistError::Io(e.to_string()));
                }
            }
        }
    }

    /// Get the remote address.
    pub fn remote_address(&self) -> SocketAddr {
        self.remote_addr
    }

    /// Close this connection.
    pub fn close(&self, _reason: &str) {
        // Dropping the stream will close it.
        // We can't actually close from a shared reference without more work.
        // In practice, dropping the TcpConnection will close the stream.
    }

    /// Check if the connection is still open.
    ///
    /// Note: This is a best-effort check. TCP connections don't provide
    /// a reliable way to check if the remote end is still connected
    /// without attempting I/O.
    pub fn is_open(&self) -> bool {
        // We can't reliably check without I/O, so assume open.
        // The next read/write will fail if it's closed.
        true
    }
}

// =============================================================================
// Transport Trait Implementations
// =============================================================================

#[async_trait]
impl Transport for TcpTransport {
    type Connection = TcpConnection;

    async fn accept(&self) -> Option<Self::Connection> {
        TcpTransport::accept(self).await
    }

    async fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<Self::Connection, DistError> {
        TcpTransport::connect(self, addr, server_name).await
    }

    fn local_addr(&self) -> Result<SocketAddr, DistError> {
        TcpTransport::local_addr(self)
    }

    fn node_name(&self) -> &str {
        TcpTransport::node_name(self)
    }

    fn creation(&self) -> u32 {
        TcpTransport::creation(self)
    }

    fn close(&self) {
        TcpTransport::close(self)
    }
}

/// Wrapper for receiving messages from a TCP connection.
pub struct TcpRecvStream {
    connection: TcpConnection,
}

#[async_trait]
impl traits::RecvStream for TcpRecvStream {
    async fn recv_message(&mut self) -> Result<DistMessage, DistError> {
        self.connection.recv_message().await
    }
}

#[async_trait]
impl TransportConnectionTrait for TcpConnection {
    type RecvStream = TcpRecvStream;

    async fn send_message(&self, msg: &DistMessage) -> Result<(), DistError> {
        TcpConnection::send_message(self, msg).await
    }

    async fn accept_and_recv(&self) -> Result<(DistMessage, Self::RecvStream), DistError> {
        // For TCP, there's no separate stream concept like QUIC.
        // We just receive on the main connection.
        let msg = self.recv_message().await?;
        Ok((
            msg,
            TcpRecvStream {
                connection: self.clone(),
            },
        ))
    }

    fn remote_address(&self) -> SocketAddr {
        TcpConnection::remote_address(self)
    }

    fn close(&self, reason: &str) {
        TcpConnection::close(self, reason)
    }

    fn is_open(&self) -> bool {
        TcpConnection::is_open(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_tcp_connection_send_recv() {
        // Set up a simple echo server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, remote_addr) = listener.accept().await.unwrap();
            let conn = TcpConnection::new(stream, remote_addr);

            // Receive and echo back
            let msg = conn.recv_message().await.unwrap();
            conn.send_message(&msg).await.unwrap();
        });

        // Client connects and sends a message
        let stream = TcpStream::connect(addr).await.unwrap();
        let conn = TcpConnection::new(stream, addr);

        let msg = DistMessage::Ping { seq: 42 };
        conn.send_message(&msg).await.unwrap();

        let response = conn.recv_message().await.unwrap();
        match response {
            DistMessage::Ping { seq } => assert_eq!(seq, 42),
            _ => panic!("unexpected message type"),
        }

        server.await.unwrap();
    }
}
