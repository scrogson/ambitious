//! Transport trait for custom channel transports.
//!
//! This module provides abstractions for building custom channel transports beyond
//! the built-in WebSocket transport. It's designed for scenarios where you need to
//! serve Phoenix-style channels over different protocols (TCP, IPC, custom).
//!
//! # When to Use This Module
//!
//! - **Use `WebSocketEndpoint`** for standard Phoenix clients (JavaScript, mobile)
//! - **Use `Transport` trait** for custom protocols (native clients, CLI tools, games)
//! - **Use `ChannelServer` directly** for complex scenarios requiring full control
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                         Transport                                    │
//! │   (WebSocket, TCP, Long Polling, Custom)                            │
//! │                                                                      │
//! │   - Accepts connections                                              │
//! │   - Handles wire framing (read/write)                                │
//! │   - Converts between wire format and TransportMessage                │
//! └─────────────────────────────────────────────────────────────────────┘
//!                                  │
//!                                  ▼
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                        ChannelServer                                 │
//! │                                                                      │
//! │   - Manages channel instances                                        │
//! │   - Routes messages to channels                                      │
//! │   - Handles join/leave lifecycle                                     │
//! └─────────────────────────────────────────────────────────────────────┘
//!                                  │
//!                                  ▼
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                          Channels                                    │
//! │              (RoomChannel, UserChannel, etc.)                        │
//! │                                                                      │
//! │   - Business logic                                                   │
//! │   - Completely transport-agnostic                                    │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example: Line-Based TCP Transport
//!
//! This example shows a simple TCP transport using newline-delimited JSON messages:
//!
//! ```ignore
//! use ambitious::channel::transport::{
//!     Transport, TransportConfig, TransportMessage, TransportReply, run_transport_loop,
//! };
//! use async_trait::async_trait;
//! use tokio::net::{TcpListener, TcpStream};
//! use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
//! use serde::{Deserialize, Serialize};
//!
//! // Our wire format: newline-delimited JSON
//! #[derive(Serialize, Deserialize)]
//! struct WireMessage {
//!     topic: String,
//!     event: String,
//!     payload: serde_json::Value,
//!     #[serde(rename = "ref")]
//!     msg_ref: Option<String>,
//! }
//!
//! struct TcpTransport {
//!     reader: BufReader<tokio::io::ReadHalf<TcpStream>>,
//!     writer: tokio::io::WriteHalf<TcpStream>,
//! }
//!
//! #[async_trait]
//! impl Transport for TcpTransport {
//!     type Connection = TcpStream;
//!     type Error = std::io::Error;
//!
//!     async fn connect(conn: Self::Connection, _config: &TransportConfig) -> Result<Self, Self::Error> {
//!         let (read, write) = tokio::io::split(conn);
//!         Ok(TcpTransport {
//!             reader: BufReader::new(read),
//!             writer: write,
//!         })
//!     }
//!
//!     async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
//!         let mut line = String::new();
//!         match self.reader.read_line(&mut line).await {
//!             Ok(0) => Ok(None), // Connection closed
//!             Ok(_) => Ok(Some(line.trim().as_bytes().to_vec())),
//!             Err(e) => Err(e),
//!         }
//!     }
//!
//!     async fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
//!         self.writer.write_all(data).await?;
//!         self.writer.write_all(b"\n").await?;
//!         self.writer.flush().await
//!     }
//!
//!     async fn close(&mut self) -> Result<(), Self::Error> {
//!         Ok(())
//!     }
//! }
//!
//! // Parser: wire bytes -> TransportMessage
//! fn parse_message(bytes: &[u8]) -> Option<TransportMessage> {
//!     let wire: WireMessage = serde_json::from_slice(bytes).ok()?;
//!     Some(TransportMessage {
//!         join_ref: None,
//!         msg_ref: wire.msg_ref,
//!         topic: wire.topic,
//!         event: wire.event,
//!         payload: serde_json::to_vec(&wire.payload).ok()?,
//!     })
//! }
//!
//! // Encoder: TransportReply -> wire bytes
//! fn encode_reply(reply: &TransportReply) -> Vec<u8> {
//!     match reply {
//!         TransportReply::Reply { topic, status, payload, msg_ref, .. } => {
//!             let response: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
//!             serde_json::to_vec(&serde_json::json!({
//!                 "topic": topic,
//!                 "event": "phx_reply",
//!                 "payload": { "status": status, "response": response },
//!                 "ref": msg_ref,
//!             })).unwrap_or_default()
//!         }
//!         TransportReply::Push { topic, event, payload, .. } => {
//!             let data: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
//!             serde_json::to_vec(&serde_json::json!({
//!                 "topic": topic,
//!                 "event": event,
//!                 "payload": data,
//!             })).unwrap_or_default()
//!         }
//!     }
//! }
//!
//! // Server setup
//! async fn run_server() {
//!     let listener = TcpListener::bind("127.0.0.1:4001").await.unwrap();
//!
//!     loop {
//!         let (stream, _) = listener.accept().await.unwrap();
//!
//!         ambitious::spawn(|| async move {
//!             let config = TransportConfig::new()
//!                 .handler::<RoomChannel>();
//!
//!             let transport = TcpTransport::connect(stream, &config).await.unwrap();
//!
//!             run_transport_loop(transport, config, parse_message, encode_reply).await.ok();
//!         });
//!     }
//! }
//! ```
//!
//! # Using ChannelServer Directly
//!
//! For more control, you can use `ChannelServer` directly without the `Transport` trait:
//!
//! ```ignore
//! use ambitious::channel::{ChannelServer, TypedChannelHandler};
//!
//! ambitious::spawn(|| async move {
//!     let pid = ambitious::current_pid();
//!     let mut server = ChannelServer::new(pid);
//!     server.add_handler::<RoomChannel>();
//!
//!     // Handle a join
//!     let reply = server.handle_join("room:lobby".into(), payload, "ref1".into()).await;
//!
//!     // Handle events
//!     if let Some(result) = server.handle_event("room:lobby".into(), "new_msg".into(), payload, "ref2".into()).await {
//!         // Process result...
//!     }
//!
//!     // Clean up
//!     server.terminate(TerminateReason::Closed).await;
//! });
//! ```

use super::{Channel, ChannelServer, DynChannelHandler, TerminateReason, TypedChannelHandler};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

// ============================================================================
// Transport Trait
// ============================================================================

/// A transport handles the low-level connection management and wire protocol.
///
/// Transports are responsible for:
/// - Accepting and establishing connections
/// - Reading raw bytes from the connection
/// - Writing raw bytes to the connection
/// - Managing connection lifecycle
///
/// Transports do NOT handle:
/// - Message parsing/serialization (that's up to the user or a Serializer)
/// - Channel routing (use `ChannelServer` for that)
/// - Channel business logic (use `Channel` for that)
///
/// # Implementing a Custom Transport
///
/// 1. Implement the `Transport` trait for your connection type
/// 2. Use `TransportConfig` to configure handlers
/// 3. Create a `ChannelServer` for managing channels
/// 4. Loop: read from transport, parse messages, call channel server methods
///
/// See the module-level documentation for a complete example.
#[async_trait]
pub trait Transport: Sized + Send {
    /// The connection type this transport wraps (e.g., TcpStream, WebSocket).
    type Connection: Send;

    /// The error type for transport operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Establish a transport connection.
    ///
    /// Called once when a new connection is accepted. Use this to perform
    /// any handshaking or initialization.
    async fn connect(conn: Self::Connection, config: &TransportConfig)
    -> Result<Self, Self::Error>;

    /// Receive raw bytes from the connection.
    ///
    /// Returns `Ok(Some(bytes))` when data is available, `Ok(None)` when the
    /// connection is closed cleanly, or `Err` on error.
    ///
    /// This should be non-blocking or have a timeout so the message loop
    /// can check for process mailbox messages.
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Send raw bytes to the connection.
    async fn send(&mut self, data: &[u8]) -> Result<(), Self::Error>;

    /// Close the connection gracefully.
    async fn close(&mut self) -> Result<(), Self::Error>;

    /// Get optional connection metadata (e.g., peer address, headers).
    fn metadata(&self) -> Option<&ConnectionMetadata> {
        None
    }

    /// Handle a control message (e.g., WebSocket ping/pong).
    ///
    /// Override this for transports that have control frames.
    async fn handle_control(&mut self, _msg: ControlMessage) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Metadata about the connection, available to channels.
#[derive(Debug, Clone, Default)]
pub struct ConnectionMetadata {
    /// Remote peer address (if available).
    pub peer_addr: Option<String>,
    /// Connection parameters (e.g., from query string).
    pub params: std::collections::HashMap<String, String>,
    /// Custom metadata set by the transport.
    pub custom: std::collections::HashMap<String, String>,
}

/// Control messages for transports that support them.
#[derive(Debug, Clone)]
pub enum ControlMessage {
    /// Ping message (WebSocket).
    Ping(Vec<u8>),
    /// Pong message (WebSocket).
    Pong(Vec<u8>),
    /// Close message with optional code and reason.
    Close(Option<u16>, Option<String>),
}

// ============================================================================
// Transport Configuration
// ============================================================================

/// Configuration for a transport connection.
#[derive(Clone)]
pub struct TransportConfig {
    /// Channel handlers registered for this transport.
    pub handlers: Vec<DynChannelHandler>,

    /// Timeout for receiving messages (for non-blocking checks).
    pub recv_timeout: Duration,

    /// Heartbeat interval (if the transport supports heartbeats).
    pub heartbeat_interval: Option<Duration>,

    /// Maximum message size (in bytes).
    pub max_message_size: Option<usize>,

    /// Custom configuration values.
    pub custom: std::collections::HashMap<String, String>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            recv_timeout: Duration::from_millis(10),
            heartbeat_interval: Some(Duration::from_secs(30)),
            max_message_size: Some(64 * 1024), // 64KB
            custom: std::collections::HashMap::new(),
        }
    }
}

impl TransportConfig {
    /// Create a new transport config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a channel handler.
    pub fn handler<C: Channel>(mut self) -> Self {
        self.handlers
            .push(Arc::new(TypedChannelHandler::<C>::new()));
        self
    }

    /// Add a pre-built handler.
    pub fn with_handler(mut self, handler: DynChannelHandler) -> Self {
        self.handlers.push(handler);
        self
    }

    /// Set the recv timeout for checking process mailbox.
    pub fn recv_timeout(mut self, timeout: Duration) -> Self {
        self.recv_timeout = timeout;
        self
    }

    /// Set the heartbeat interval.
    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = Some(interval);
        self
    }

    /// Disable heartbeats.
    pub fn no_heartbeat(mut self) -> Self {
        self.heartbeat_interval = None;
        self
    }

    /// Set maximum message size.
    pub fn max_message_size(mut self, size: usize) -> Self {
        self.max_message_size = Some(size);
        self
    }

    /// Build a ChannelServer with the configured handlers.
    ///
    /// Call this from within an Ambitious process to get the current PID.
    pub fn build_server(&self) -> ChannelServer {
        let pid = crate::current_pid();
        let mut server = ChannelServer::new(pid);
        for handler in &self.handlers {
            server.add_dyn_handler(handler.clone());
        }
        server
    }
}

// ============================================================================
// Transport Message Types
// ============================================================================

/// A parsed message from the transport.
///
/// After deserializing from the wire format, messages are represented
/// in this normalized form for the channel server.
#[derive(Debug, Clone)]
pub struct TransportMessage {
    /// Join reference for channel correlation.
    pub join_ref: Option<String>,
    /// Message reference for request/reply correlation.
    pub msg_ref: Option<String>,
    /// The topic (e.g., "room:lobby").
    pub topic: String,
    /// The event name (e.g., "phx_join", "new_msg").
    pub event: String,
    /// The payload bytes.
    pub payload: Vec<u8>,
}

/// Result of handling a transport message.
#[derive(Debug)]
pub enum TransportResult {
    /// No response needed.
    Ok,
    /// Send a reply to the client.
    Reply(Vec<u8>),
    /// Send multiple messages to the client.
    Replies(Vec<Vec<u8>>),
    /// Close the connection.
    Close,
    /// Close with an error.
    Error(String),
}

/// Reply types for encoding outgoing messages.
#[derive(Debug, Clone)]
pub enum TransportReply {
    /// Reply to a request.
    Reply {
        /// Join reference for correlation.
        join_ref: Option<String>,
        /// Message reference for correlation.
        msg_ref: Option<String>,
        /// Topic.
        topic: String,
        /// Status (ok/error).
        status: String,
        /// Payload bytes.
        payload: Vec<u8>,
    },
    /// Push a message to the client.
    Push {
        /// Join reference for correlation.
        join_ref: Option<String>,
        /// Topic.
        topic: String,
        /// Event name.
        event: String,
        /// Payload bytes.
        payload: Vec<u8>,
    },
}

// ============================================================================
// Transport Builder
// ============================================================================

/// Builder for creating transport configurations.
///
/// This provides a fluent API for configuring transports.
///
/// # Example
///
/// ```ignore
/// let config = TransportBuilder::new()
///     .channel::<RoomChannel>()
///     .channel::<UserChannel>()
///     .heartbeat_interval(Duration::from_secs(30))
///     .build();
///
/// // Use config to create a ChannelServer
/// let server = config.build_server();
/// ```
pub struct TransportBuilder {
    config: TransportConfig,
}

impl TransportBuilder {
    /// Create a new transport builder.
    pub fn new() -> Self {
        Self {
            config: TransportConfig::default(),
        }
    }

    /// Register a channel.
    pub fn channel<C: Channel>(mut self) -> Self {
        self.config = self.config.handler::<C>();
        self
    }

    /// Add a pre-built handler.
    pub fn handler(mut self, handler: DynChannelHandler) -> Self {
        self.config = self.config.with_handler(handler);
        self
    }

    /// Set heartbeat interval.
    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.config = self.config.heartbeat_interval(interval);
        self
    }

    /// Disable heartbeats.
    pub fn no_heartbeat(mut self) -> Self {
        self.config = self.config.no_heartbeat();
        self
    }

    /// Set max message size.
    pub fn max_message_size(mut self, size: usize) -> Self {
        self.config = self.config.max_message_size(size);
        self
    }

    /// Set recv timeout.
    pub fn recv_timeout(mut self, timeout: Duration) -> Self {
        self.config = self.config.recv_timeout(timeout);
        self
    }

    /// Build the transport config.
    pub fn build(self) -> TransportConfig {
        self.config
    }
}

impl Default for TransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Convenience function for simple transports
// ============================================================================

/// Run a simple transport loop.
///
/// This is a convenience function for transports that follow a simple pattern:
/// 1. Read message from transport
/// 2. Parse into TransportMessage
/// 3. Route through ChannelServer
/// 4. Encode reply and send
///
/// For more complex transports (like WebSocket with multiplexed messages),
/// implement the loop manually using `ChannelServer` directly.
///
/// # Arguments
///
/// * `transport` - The transport connection
/// * `config` - Transport configuration with channel handlers
/// * `parser` - Function to parse raw bytes into a TransportMessage
/// * `encoder` - Function to encode a TransportReply into bytes
///
/// # Example
///
/// ```ignore
/// use ambitious::channel::transport::*;
///
/// async fn handle_connection(stream: TcpStream) {
///     let config = TransportBuilder::new()
///         .channel::<RoomChannel>()
///         .build();
///
///     let transport = MyTcpTransport::connect(stream, &config).await.unwrap();
///
///     run_transport_loop(
///         transport,
///         config,
///         |bytes| parse_message(bytes),
///         |reply| encode_reply(reply),
///     ).await;
/// }
/// ```
pub async fn run_transport_loop<T, P, E>(
    mut transport: T,
    config: TransportConfig,
    parser: P,
    encoder: E,
) -> Result<(), T::Error>
where
    T: Transport,
    P: Fn(&[u8]) -> Option<TransportMessage> + Send,
    E: Fn(&TransportReply) -> Vec<u8> + Send,
{
    let mut server = config.build_server();

    loop {
        tokio::select! {
            biased;

            // Check process mailbox for broadcasts (non-blocking)
            mailbox_result = crate::recv_timeout(config.recv_timeout) => {
                if let Ok(Some(msg_bytes)) = mailbox_result {
                    // Try to decode as a ChannelReply::Push (broadcast from another client)
                    if let Ok(super::ChannelReply::Push { topic, event, payload }) =
                        postcard::from_bytes::<super::ChannelReply>(&msg_bytes)
                    {
                        // Forward to client
                        let reply = TransportReply::Push {
                            join_ref: None,
                            topic,
                            event,
                            payload,
                        };
                        transport.send(&encoder(&reply)).await?;
                    }
                    // Also dispatch to handle_info for internal messages
                    let _results = server.handle_info_any(crate::RawTerm::from(msg_bytes)).await;
                }
            }

            // Check for incoming transport data
            recv_result = transport.recv() => {
                match recv_result {
                    Ok(Some(data)) => {
                        if let Some(msg) = parser(&data) {
                            let replies = handle_message(&mut server, &msg, &encoder).await;
                            for reply_bytes in replies {
                                transport.send(&reply_bytes).await?;
                            }
                        }
                    }
                    Ok(None) => {
                        // Connection closed cleanly
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Transport recv error");
                        break;
                    }
                }
            }
        }
    }

    // Cleanup: terminate all channels
    server.terminate(TerminateReason::Closed).await;
    transport.close().await?;
    Ok(())
}

/// Handle a single message and return encoded replies.
async fn handle_message<E>(
    server: &mut ChannelServer,
    msg: &TransportMessage,
    encoder: &E,
) -> Vec<Vec<u8>>
where
    E: Fn(&TransportReply) -> Vec<u8>,
{
    let mut replies = Vec::new();

    match msg.event.as_str() {
        "phx_join" => {
            let reply = server
                .handle_join(
                    msg.topic.clone(),
                    msg.payload.clone(),
                    msg.msg_ref.clone().unwrap_or_default(),
                )
                .await;

            let (status, payload) = match reply {
                super::ChannelReply::JoinOk { payload, .. } => {
                    ("ok", payload.unwrap_or_else(|| b"{}".to_vec()))
                }
                super::ChannelReply::JoinError { reason, .. } => (
                    "error",
                    format!(r#"{{"reason":"{}"}}"#, reason).into_bytes(),
                ),
                _ => ("ok", b"{}".to_vec()),
            };

            replies.push(encoder(&TransportReply::Reply {
                join_ref: msg.join_ref.clone(),
                msg_ref: msg.msg_ref.clone(),
                topic: msg.topic.clone(),
                status: status.to_string(),
                payload,
            }));
        }
        "phx_leave" => {
            server.handle_leave(msg.topic.clone()).await;
            replies.push(encoder(&TransportReply::Reply {
                join_ref: msg.join_ref.clone(),
                msg_ref: msg.msg_ref.clone(),
                topic: msg.topic.clone(),
                status: "ok".to_string(),
                payload: b"{}".to_vec(),
            }));
        }
        "heartbeat" if msg.topic == "phoenix" => {
            replies.push(encoder(&TransportReply::Reply {
                join_ref: None,
                msg_ref: msg.msg_ref.clone(),
                topic: "phoenix".to_string(),
                status: "ok".to_string(),
                payload: b"{}".to_vec(),
            }));
        }
        _ => {
            // Regular event
            if let Some(reply) = server
                .handle_event(
                    msg.topic.clone(),
                    msg.event.clone(),
                    msg.payload.clone(),
                    msg.msg_ref.clone().unwrap_or_default(),
                )
                .await
            {
                match reply {
                    super::ChannelReply::Reply {
                        msg_ref,
                        status,
                        payload,
                    } => {
                        replies.push(encoder(&TransportReply::Reply {
                            join_ref: msg.join_ref.clone(),
                            msg_ref: Some(msg_ref),
                            topic: msg.topic.clone(),
                            status,
                            payload,
                        }));
                    }
                    super::ChannelReply::Push {
                        topic,
                        event,
                        payload,
                    } => {
                        replies.push(encoder(&TransportReply::Push {
                            join_ref: msg.join_ref.clone(),
                            topic,
                            event,
                            payload,
                        }));
                    }
                    _ => {}
                }
            }
        }
    }

    replies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_config_builder() {
        let config = TransportConfig::new()
            .recv_timeout(Duration::from_millis(50))
            .heartbeat_interval(Duration::from_secs(60))
            .max_message_size(128 * 1024);

        assert_eq!(config.recv_timeout, Duration::from_millis(50));
        assert_eq!(config.heartbeat_interval, Some(Duration::from_secs(60)));
        assert_eq!(config.max_message_size, Some(128 * 1024));
    }

    #[test]
    fn test_transport_builder() {
        let config = TransportBuilder::new()
            .heartbeat_interval(Duration::from_secs(45))
            .max_message_size(32 * 1024)
            .build();

        assert_eq!(config.heartbeat_interval, Some(Duration::from_secs(45)));
        assert_eq!(config.max_message_size, Some(32 * 1024));
    }
}
