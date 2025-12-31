//! WebSocket transport for Phoenix Channels V2 protocol.
//!
//! This module provides a WebSocket server that speaks the Phoenix Channels V2
//! JSON protocol, allowing standard Phoenix JavaScript clients to connect directly.
//!
//! # Protocol
//!
//! Messages are JSON arrays: `[join_ref, ref, topic, event, payload]`
//!
//! Special events:
//! - `phx_join`: Join a channel topic
//! - `phx_leave`: Leave a channel topic
//! - `heartbeat`: Keep connection alive (topic: "phoenix")
//! - `phx_reply`: Server reply to client request
//! - `phx_error`: Error response
//! - `phx_close`: Channel closed
//!
//! # Serializer Pattern
//!
//! Following Phoenix's approach, serialization is handled at the transport boundary
//! by a `Serializer` trait. The WebSocket transport uses `V2JsonSerializer` by default.
//! Channels are completely unaware of the wire format.
//!
//! # Example
//!
//! ```ignore
//! use ambitious::channel::websocket::{WebSocketEndpoint, WebSocketConfig};
//! use ambitious::channel::{Channel, ChannelServerBuilder};
//!
//! // Create endpoint with your channels
//! let endpoint = WebSocketEndpoint::new()
//!     .channel::<RoomChannel>()
//!     .channel::<UserChannel>();
//!
//! // Start serving
//! let addr = "127.0.0.1:4000".parse().unwrap();
//! endpoint.listen(addr).await?;
//! ```

use super::{
    Channel, ChannelHandler, ChannelInstance, ChannelReply, JoinError, RawHandleResult,
    ReplyStatus, Socket, TerminateReason, TypedChannelHandler,
};
use crate::Pid;
use crate::dist::pg;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

// ============================================================================
// Serializer - Phoenix-style transport boundary serialization
// ============================================================================

/// A serializer handles encoding/decoding messages at the transport boundary.
///
/// This follows Phoenix's Serializer behavior pattern. The serializer is
/// responsible for converting between wire format (e.g., JSON) and the
/// internal message representation.
///
/// Channels never see wire formats - they work with typed Rust structs.
/// The serializer sits at the edge of the system.
pub trait Serializer: Send + Sync {
    /// Decode incoming wire data into a Phoenix message.
    ///
    /// Returns the parsed message components: join_ref, msg_ref, topic, event, payload.
    /// The payload is returned as raw bytes in the wire format.
    fn decode(&self, data: &[u8]) -> Option<DecodedMessage>;

    /// Encode a reply message to wire format.
    fn encode_reply(
        &self,
        join_ref: Option<&str>,
        msg_ref: Option<&str>,
        topic: &str,
        status: &str,
        payload: &[u8],
    ) -> Vec<u8>;

    /// Encode a push message to wire format.
    fn encode_push(
        &self,
        join_ref: Option<&str>,
        topic: &str,
        event: &str,
        payload: &[u8],
    ) -> Vec<u8>;

    /// Fast path for encoding broadcast messages.
    ///
    /// This is called for high-frequency broadcasts. Implementations may
    /// optimize this path (e.g., pre-serializing parts of the message).
    fn fastlane(&self, topic: &str, event: &str, payload: &[u8]) -> Vec<u8> {
        self.encode_push(None, topic, event, payload)
    }
}

/// A decoded message from the wire.
#[derive(Debug, Clone)]
pub struct DecodedMessage {
    /// Join reference for channel correlation.
    pub join_ref: Option<String>,
    /// Message reference for request/reply correlation.
    pub msg_ref: Option<String>,
    /// The topic (e.g., "room:lobby").
    pub topic: String,
    /// The event name (e.g., "phx_join", "new_msg").
    pub event: String,
    /// The payload in wire format (e.g., JSON bytes).
    pub payload: Vec<u8>,
}

/// A factory for creating serializers.
///
/// This allows dynamic serializer selection at connection time based on
/// connection parameters like the `vsn` query param.
pub type SerializerFactory = Arc<dyn Fn() -> Arc<dyn Serializer> + Send + Sync>;

/// Registry for serializer versions.
///
/// Maps version strings (from `vsn` query param) to serializer factories.
/// The endpoint uses this to negotiate the wire format per-connection.
///
/// # Example
///
/// ```ignore
/// let registry = SerializerRegistry::new()
///     .register("1.0.0", || Arc::new(V1JsonSerializer))
///     .register("2.0.0", || Arc::new(V2JsonSerializer))
///     .register("postcard", || Arc::new(PostcardSerializer))
///     .default_version("2.0.0");
/// ```
pub struct SerializerRegistry {
    serializers: HashMap<String, SerializerFactory>,
    default_version: String,
}

impl SerializerRegistry {
    /// Create a new registry with V2JsonSerializer as default.
    pub fn new() -> Self {
        let mut serializers = HashMap::new();
        serializers.insert(
            "2.0.0".to_string(),
            Arc::new(|| Arc::new(V2JsonSerializer) as Arc<dyn Serializer>) as SerializerFactory,
        );
        Self {
            serializers,
            default_version: "2.0.0".to_string(),
        }
    }

    /// Register a serializer for a version string.
    pub fn register<F>(mut self, version: &str, factory: F) -> Self
    where
        F: Fn() -> Arc<dyn Serializer> + Send + Sync + 'static,
    {
        self.serializers
            .insert(version.to_string(), Arc::new(factory));
        self
    }

    /// Set the default version to use when `vsn` is not specified.
    pub fn default_version(mut self, version: &str) -> Self {
        self.default_version = version.to_string();
        self
    }

    /// Get a serializer for the given version.
    ///
    /// Returns the default serializer if the version is not registered.
    pub fn get(&self, version: Option<&str>) -> Arc<dyn Serializer> {
        let vsn = version.unwrap_or(&self.default_version);
        self.serializers.get(vsn).map(|f| f()).unwrap_or_else(|| {
            // Fall back to default
            self.serializers
                .get(&self.default_version)
                .map(|f| f())
                .unwrap_or_else(|| Arc::new(V2JsonSerializer))
        })
    }

    /// Get the list of supported versions.
    pub fn versions(&self) -> Vec<&str> {
        self.serializers.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for SerializerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Phoenix Channels V2 JSON serializer.
///
/// This implements the standard Phoenix Channels V2 protocol:
/// - Wire format: `[join_ref, ref, topic, event, payload]`
/// - Payload encoding: JSON
///
/// This is the default serializer for WebSocket connections.
#[derive(Debug, Clone, Default)]
pub struct V2JsonSerializer;

impl Serializer for V2JsonSerializer {
    fn decode(&self, data: &[u8]) -> Option<DecodedMessage> {
        let text = std::str::from_utf8(data).ok()?;
        let value: Value = serde_json::from_str(text).ok()?;
        let arr = value.as_array()?;
        if arr.len() != 5 {
            return None;
        }

        Some(DecodedMessage {
            join_ref: arr[0].as_str().map(String::from),
            msg_ref: arr[1].as_str().map(String::from),
            topic: arr[2].as_str()?.to_string(),
            event: arr[3].as_str()?.to_string(),
            payload: serde_json::to_vec(&arr[4]).ok()?,
        })
    }

    fn encode_reply(
        &self,
        join_ref: Option<&str>,
        msg_ref: Option<&str>,
        topic: &str,
        status: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        // Parse payload as JSON, or use empty object
        let response: Value = serde_json::from_slice(payload).unwrap_or(json!({}));
        let msg = json!([
            join_ref,
            msg_ref,
            topic,
            "phx_reply",
            { "status": status, "response": response }
        ]);
        msg.to_string().into_bytes()
    }

    fn encode_push(
        &self,
        join_ref: Option<&str>,
        topic: &str,
        event: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        // Parse payload as JSON, or wrap in object
        let payload_json: Value =
            serde_json::from_slice(payload).unwrap_or_else(|_| json!({ "data": payload }));
        let msg = json!([join_ref, null, topic, event, payload_json]);
        msg.to_string().into_bytes()
    }
}

/// Phoenix Channels V2 message format.
/// Format: [join_ref, ref, topic, event, payload]
#[derive(Debug, Clone)]
pub struct PhxMessage {
    /// Join reference for correlating messages within a channel.
    pub join_ref: Option<String>,
    /// Message reference for request/reply correlation.
    pub msg_ref: Option<String>,
    /// The topic (e.g., "room:lobby").
    pub topic: String,
    /// The event name (e.g., "phx_join", "new_msg").
    pub event: String,
    /// The payload as JSON.
    pub payload: Value,
}

impl PhxMessage {
    /// Create a new Phoenix message.
    pub fn new(topic: impl Into<String>, event: impl Into<String>, payload: Value) -> Self {
        Self {
            join_ref: None,
            msg_ref: None,
            topic: topic.into(),
            event: event.into(),
            payload,
        }
    }

    /// Set the join reference.
    pub fn with_join_ref(mut self, join_ref: impl Into<String>) -> Self {
        self.join_ref = Some(join_ref.into());
        self
    }

    /// Set the message reference.
    pub fn with_msg_ref(mut self, msg_ref: impl Into<String>) -> Self {
        self.msg_ref = Some(msg_ref.into());
        self
    }

    /// Parse a V2 protocol message from JSON array.
    pub fn from_json(value: &Value) -> Option<Self> {
        let arr = value.as_array()?;
        if arr.len() != 5 {
            return None;
        }

        Some(PhxMessage {
            join_ref: arr[0].as_str().map(String::from),
            msg_ref: arr[1].as_str().map(String::from),
            topic: arr[2].as_str()?.to_string(),
            event: arr[3].as_str()?.to_string(),
            payload: arr[4].clone(),
        })
    }

    /// Serialize to V2 protocol JSON array.
    pub fn to_json(&self) -> Value {
        json!([
            self.join_ref,
            self.msg_ref,
            self.topic,
            self.event,
            self.payload
        ])
    }

    /// Create a reply message.
    pub fn reply(
        join_ref: Option<String>,
        msg_ref: Option<String>,
        topic: impl Into<String>,
        status: &str,
        response: Value,
    ) -> Self {
        Self {
            join_ref,
            msg_ref,
            topic: topic.into(),
            event: "phx_reply".to_string(),
            payload: json!({ "status": status, "response": response }),
        }
    }

    /// Create an ok reply.
    pub fn ok_reply(
        join_ref: Option<String>,
        msg_ref: Option<String>,
        topic: impl Into<String>,
        response: Value,
    ) -> Self {
        Self::reply(join_ref, msg_ref, topic, "ok", response)
    }

    /// Create an error reply.
    pub fn error_reply(
        join_ref: Option<String>,
        msg_ref: Option<String>,
        topic: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::reply(
            join_ref,
            msg_ref,
            topic,
            "error",
            json!({ "reason": reason.into() }),
        )
    }

    /// Create a push message (server-initiated).
    pub fn push(
        join_ref: Option<String>,
        topic: impl Into<String>,
        event: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self {
            join_ref,
            msg_ref: None,
            topic: topic.into(),
            event: event.into(),
            payload,
        }
    }
}

/// Configuration for the WebSocket endpoint.
#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    /// Maximum message size in bytes.
    pub max_message_size: usize,
    /// Heartbeat interval timeout in seconds.
    pub heartbeat_timeout: u64,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            max_message_size: 64 * 1024, // 64KB
            heartbeat_timeout: 60,
        }
    }
}

// ============================================================================
// JSON Channel Handler for WebSocket transport
// ============================================================================

/// A channel handler that uses JSON for wire format payloads.
///
/// This wraps a standard channel and handles JSON<->postcard conversion
/// at the join boundary. The Serializer handles the rest at the transport layer.
pub struct JsonChannelHandler<C: Channel> {
    _phantom: std::marker::PhantomData<C>,
}

impl<C: Channel> JsonChannelHandler<C> {
    /// Create a new JSON channel handler.
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<C: Channel> Default for JsonChannelHandler<C> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<C: Channel> ChannelHandler for JsonChannelHandler<C>
where
    C::Join: DeserializeOwned + serde::Serialize,
    C::In: DeserializeOwned + serde::Serialize,
    C::Out: DeserializeOwned + serde::Serialize,
{
    fn topic_pattern(&self) -> &'static str {
        C::TOPIC_PATTERN
    }

    fn matches(&self, topic: &str) -> bool {
        super::topic_matches(C::TOPIC_PATTERN, topic)
    }

    async fn handle_join(
        &self,
        topic: &str,
        payload: &[u8],
        socket: Socket,
    ) -> Result<(Box<dyn ChannelInstance>, Option<Vec<u8>>), JoinError> {
        // Parse JSON payload into the channel's Join type
        let join_payload: C::Join = serde_json::from_slice(payload)
            .map_err(|e| JoinError::new(format!("invalid payload: {}", e)))?;

        match C::join(topic, join_payload, &socket).await {
            super::JoinResult::Ok(channel) => {
                Ok((Box::new(JsonChannelInstance::new(channel)), None))
            }
            super::JoinResult::OkReply(channel, reply) => {
                // Reply is already in internal format, convert to JSON for wire
                let json_reply = serde_json::to_vec(&serde_json::Value::Object(
                    serde_json::from_slice(&reply).unwrap_or_default(),
                ))
                .ok();
                Ok((Box::new(JsonChannelInstance::new(channel)), json_reply))
            }
            super::JoinResult::Error(e) => Err(e),
        }
    }
}

/// A JSON-aware channel instance wrapper.
///
/// Converts between JSON (wire) and postcard (internal) at the channel boundary.
struct JsonChannelInstance<C: Channel> {
    channel: C,
}

impl<C: Channel> JsonChannelInstance<C> {
    fn new(channel: C) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl<C: Channel> ChannelInstance for JsonChannelInstance<C>
where
    C::In: DeserializeOwned + serde::Serialize,
    C::Out: DeserializeOwned + serde::Serialize,
    C::Join: DeserializeOwned + serde::Serialize,
{
    async fn handle_event(
        &mut self,
        event: &str,
        payload: &[u8],
        socket: &Socket,
    ) -> RawHandleResult {
        // Convert JSON payload to internal format (postcard)
        let internal_payload: Vec<u8> = match serde_json::from_slice::<C::In>(payload) {
            Ok(msg) => match postcard::to_allocvec(&msg) {
                Ok(bytes) => bytes,
                Err(_) => payload.to_vec(),
            },
            Err(_) => payload.to_vec(),
        };

        let result = self
            .channel
            .handle_in_raw(event, &internal_payload, socket)
            .await;

        // Convert result payloads from internal to JSON
        self.convert_result_to_json(result)
    }

    async fn handle_info(&mut self, msg: crate::core::RawTerm, socket: &Socket) -> RawHandleResult {
        let result = self.channel.handle_info_raw(msg, socket).await;
        self.convert_result_to_json(result)
    }

    async fn terminate(&mut self, reason: TerminateReason, socket: &Socket) {
        self.channel.terminate(reason, socket).await;
    }

    fn is_intercepted(&self, event: &str) -> bool {
        C::is_intercepted(event)
    }

    async fn handle_out(
        &mut self,
        event: &str,
        payload: &[u8],
        socket: &Socket,
    ) -> crate::channel::OutResult {
        self.channel.handle_out_raw(event, payload, socket).await
    }
}

impl<C: Channel> JsonChannelInstance<C>
where
    C::Out: DeserializeOwned + serde::Serialize,
{
    /// Convert internal format payloads in a result to JSON for wire transmission.
    fn convert_result_to_json(&self, result: RawHandleResult) -> RawHandleResult {
        match result {
            RawHandleResult::Reply { status, payload } => {
                let json_payload = self.payload_to_json(&payload).unwrap_or(payload);
                RawHandleResult::Reply {
                    status,
                    payload: json_payload,
                }
            }
            RawHandleResult::Push { event, payload } => {
                let json_payload = self.payload_to_json(&payload).unwrap_or(payload);
                RawHandleResult::Push {
                    event,
                    payload: json_payload,
                }
            }
            RawHandleResult::Broadcast { event, payload } => {
                let json_payload = self.payload_to_json(&payload).unwrap_or(payload);
                RawHandleResult::Broadcast {
                    event,
                    payload: json_payload,
                }
            }
            RawHandleResult::BroadcastFrom { event, payload } => {
                let json_payload = self.payload_to_json(&payload).unwrap_or(payload);
                RawHandleResult::BroadcastFrom {
                    event,
                    payload: json_payload,
                }
            }
            other => other,
        }
    }

    /// Convert internal (postcard) payload to JSON.
    fn payload_to_json(&self, payload: &[u8]) -> Option<Vec<u8>> {
        // Try to decode as C::Out and re-encode as JSON
        let out: C::Out = postcard::from_bytes(payload).ok()?;
        serde_json::to_vec(&out).ok()
    }
}

/// A WebSocket endpoint that serves Phoenix Channels.
///
/// The endpoint manages WebSocket connections and routes messages to the
/// appropriate channel handlers. It uses a `SerializerRegistry` to negotiate
/// the wire format per-connection based on the `vsn` query parameter.
///
/// # Serializer Negotiation
///
/// Clients can specify the protocol version via query param:
/// - `ws://localhost:4000/socket/websocket?vsn=2.0.0` - Phoenix V2 JSON
/// - `ws://localhost:4000/socket/websocket?vsn=postcard` - Binary postcard format
///
/// Custom serializers can be registered for different clients (TUI, native, etc).
pub struct WebSocketEndpoint {
    handlers: Vec<Arc<dyn ChannelHandler + Send + Sync>>,
    config: WebSocketConfig,
    serializer_registry: Arc<SerializerRegistry>,
}

impl WebSocketEndpoint {
    /// Create a new WebSocket endpoint with the default serializer registry.
    ///
    /// The default registry supports:
    /// - `2.0.0` - Phoenix Channels V2 JSON protocol (default)
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            config: WebSocketConfig::default(),
            serializer_registry: Arc::new(SerializerRegistry::new()),
        }
    }

    /// Set the configuration.
    pub fn config(mut self, config: WebSocketConfig) -> Self {
        self.config = config;
        self
    }

    /// Set a custom serializer registry for version negotiation.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let registry = SerializerRegistry::new()
    ///     .register("2.0.0", || Arc::new(V2JsonSerializer))
    ///     .register("postcard", || Arc::new(PostcardSerializer))
    ///     .default_version("2.0.0");
    ///
    /// let endpoint = WebSocketEndpoint::new()
    ///     .serializer_registry(registry)
    ///     .channel::<RoomChannel>();
    /// ```
    pub fn serializer_registry(mut self, registry: SerializerRegistry) -> Self {
        self.serializer_registry = Arc::new(registry);
        self
    }

    /// Set a single serializer for all connections (bypasses version negotiation).
    ///
    /// Use this for simple setups where all clients use the same protocol.
    /// For multi-protocol support, use `serializer_registry` instead.
    pub fn serializer<S: Serializer + 'static>(mut self, serializer: S) -> Self {
        // Create a registry with only this serializer
        let s = Arc::new(serializer);
        let registry = SerializerRegistry::new().register("default", move || s.clone());
        self.serializer_registry = Arc::new(registry.default_version("default"));
        self
    }

    /// Register a channel handler using JSON serialization.
    ///
    /// The channel will use JSON serialization for payloads, which is
    /// required for the Phoenix Channels V2 protocol.
    pub fn channel<C>(mut self) -> Self
    where
        C: Channel,
        C::Join: DeserializeOwned + serde::Serialize,
        C::In: DeserializeOwned + serde::Serialize,
        C::Out: DeserializeOwned + serde::Serialize,
    {
        self.handlers.push(Arc::new(JsonChannelHandler::<C>::new()));
        self
    }

    /// Register a channel handler using postcard (binary) serialization.
    ///
    /// Use this for channels that communicate with non-WebSocket clients.
    pub fn channel_postcard<C>(mut self) -> Self
    where
        C: Channel,
    {
        self.handlers
            .push(Arc::new(TypedChannelHandler::<C>::new()));
        self
    }

    /// Start listening for WebSocket connections.
    pub async fn listen(
        self,
        addr: SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(%addr, "WebSocket endpoint listening");

        let endpoint = Arc::new(self);

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let endpoint = endpoint.clone();
            // Spawn as a Ambitious process so we have a valid PID context
            crate::spawn(move || async move {
                if let Err(e) = handle_connection(endpoint, stream, peer_addr).await {
                    tracing::warn!(%peer_addr, error = %e, "WebSocket connection error");
                }
            });
        }
    }

    /// Get the registered handlers.
    pub fn handlers(&self) -> &[Arc<dyn ChannelHandler + Send + Sync>] {
        &self.handlers
    }

    /// Get the serializer registry.
    pub fn registry(&self) -> &Arc<SerializerRegistry> {
        &self.serializer_registry
    }

    /// Get a serializer for a specific version.
    pub fn get_serializer(&self, version: Option<&str>) -> Arc<dyn Serializer> {
        self.serializer_registry.get(version)
    }
}

impl Default for WebSocketEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// Active channel connection in WebSocket session.
struct WsChannelConnection {
    socket: Socket,
    instance: Box<dyn ChannelInstance>,
    join_ref: Option<String>,
}

/// WebSocket session state.
struct WsSession {
    /// Process ID for this session.
    pid: Pid,
    /// Registered channel handlers.
    handlers: Vec<Arc<dyn ChannelHandler + Send + Sync>>,
    /// Active channel connections: topic -> connection.
    channels: HashMap<String, WsChannelConnection>,
}

impl WsSession {
    fn new(pid: Pid, handlers: Vec<Arc<dyn ChannelHandler + Send + Sync>>) -> Self {
        Self {
            pid,
            handlers,
            channels: HashMap::new(),
        }
    }

    fn find_handler(&self, topic: &str) -> Option<&Arc<dyn ChannelHandler + Send + Sync>> {
        self.handlers.iter().find(|h| h.matches(topic))
    }

    async fn handle_join(
        &mut self,
        topic: String,
        payload: Vec<u8>,
        join_ref: Option<String>,
    ) -> Result<Option<Vec<u8>>, JoinError> {
        // Check if already joined
        if self.channels.contains_key(&topic) {
            return Err(JoinError::new("already joined"));
        }

        // Find handler
        let handler = self
            .find_handler(&topic)
            .ok_or_else(|| JoinError::new(format!("no handler for topic: {}", topic)))?
            .clone();

        // Create socket
        let socket = Socket::new(self.pid, &topic);

        // Call join
        let (instance, reply_payload) = handler
            .handle_join(&topic, &payload, socket.clone())
            .await?;

        // Subscribe to topic via pg
        let group = format!("channel:{}", topic);
        pg::join(&group, self.pid);

        // Store the channel connection
        self.channels.insert(
            topic,
            WsChannelConnection {
                socket,
                instance,
                join_ref,
            },
        );

        Ok(reply_payload)
    }

    async fn handle_event(
        &mut self,
        topic: &str,
        event: &str,
        payload: Vec<u8>,
    ) -> Option<RawHandleResult> {
        let conn = self.channels.get_mut(topic)?;
        Some(
            conn.instance
                .handle_event(event, &payload, &conn.socket)
                .await,
        )
    }

    async fn handle_leave(&mut self, topic: &str) {
        if let Some(mut conn) = self.channels.remove(topic) {
            conn.instance
                .terminate(TerminateReason::Left, &conn.socket)
                .await;

            // Unsubscribe from pg
            let group = format!("channel:{}", topic);
            pg::leave(&group, self.pid);
        }
    }

    async fn handle_info_all(
        &mut self,
        msg: crate::core::RawTerm,
    ) -> Vec<(String, RawHandleResult)> {
        let mut results = Vec::new();
        let topics: Vec<String> = self.channels.keys().cloned().collect();

        for topic in topics {
            if let Some(conn) = self.channels.get_mut(&topic) {
                let result = conn.instance.handle_info(msg.clone(), &conn.socket).await;
                results.push((topic, result));
            }
        }

        results
    }

    fn is_joined(&self, topic: &str) -> bool {
        self.channels.contains_key(topic)
    }

    fn get_join_ref(&self, topic: &str) -> Option<String> {
        self.channels.get(topic).and_then(|c| c.join_ref.clone())
    }

    async fn terminate(&mut self) {
        let topics: Vec<String> = self.channels.keys().cloned().collect();
        for topic in topics {
            if let Some(mut conn) = self.channels.remove(&topic) {
                conn.instance
                    .terminate(TerminateReason::Closed, &conn.socket)
                    .await;
                let group = format!("channel:{}", topic);
                pg::leave(&group, self.pid);
            }
        }
    }
}

/// Extract query parameters from a URI string.
fn parse_query_params(uri: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Some(query_start) = uri.find('?') {
        let query = &uri[query_start + 1..];
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                params.insert(key.to_string(), value.to_string());
            }
        }
    }
    params
}

/// Handle a WebSocket connection.
async fn handle_connection(
    endpoint: Arc<WebSocketEndpoint>,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    // Track the vsn from the HTTP request
    let vsn: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let vsn_capture = vsn.clone();

    // Accept WebSocket with header callback to extract vsn
    let callback = |req: &Request,
                    response: Response|
     -> Result<
        Response,
        tokio_tungstenite::tungstenite::http::Response<Option<String>>,
    > {
        let uri = req.uri().to_string();
        let params = parse_query_params(&uri);
        if let Some(v) = params.get("vsn")
            && let Ok(mut guard) = vsn_capture.lock()
        {
            *guard = Some(v.clone());
        }
        tracing::info!(%addr, uri = %uri, vsn = ?params.get("vsn"), "WebSocket connection");
        Ok(response)
    };

    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, callback).await?;
    let (mut write, mut read) = ws_stream.split();

    // Get a PID for this session
    let pid = crate::current_pid();

    // Get the serializer based on negotiated vsn
    let negotiated_vsn = vsn.lock().ok().and_then(|g| g.clone());
    let serializer = endpoint.get_serializer(negotiated_vsn.as_deref());
    tracing::debug!(%addr, vsn = ?negotiated_vsn, "Selected serializer");

    let mut session = WsSession::new(pid, endpoint.handlers.clone());

    loop {
        tokio::select! {
            biased;

            // Handle incoming Ambitious messages (broadcasts from other clients, or internal messages)
            // Check this first since broadcasts should be delivered promptly
            mailbox_result = crate::recv_timeout(Duration::from_millis(10)) => {
                if let Ok(Some(msg_bytes)) = mailbox_result {
                    // Try to decode as a ChannelReply (broadcast)
                    if let Ok(channel_reply) = postcard::from_bytes::<ChannelReply>(&msg_bytes) {
                        if let ChannelReply::Push { topic, event, payload } = channel_reply {
                            // Also dispatch to handle_info so channel can respond to presence sync, etc.
                            let info_results = session.handle_info_all(msg_bytes.clone().into()).await;
                            for (info_topic, result) in info_results {
                                if let RawHandleResult::Broadcast { event: bc_event, payload: bc_payload } = result {
                                    let group = format!("channel:{}", info_topic);
                                    let msg = ChannelReply::Push {
                                        topic: info_topic.clone(),
                                        event: bc_event,
                                        payload: bc_payload,
                                    };
                                    if let Ok(bytes) = postcard::to_allocvec(&msg) {
                                        let members = pg::get_members(&group);
                                        for member_pid in members {
                                            let _ = crate::send_raw(member_pid, bytes.clone());
                                        }
                                    }
                                }
                            }

                            // Use the serializer to encode the push message for the wire
                            let wire_bytes = serializer.encode_push(
                                session.get_join_ref(&topic).as_deref(),
                                &topic,
                                &event,
                                &payload,
                            );

                            if let Err(e) = write.send(Message::Text(String::from_utf8_lossy(&wire_bytes).into_owned().into())).await {
                                tracing::warn!(%addr, error = %e, "WebSocket write error");
                                break;
                            }
                        }
                    } else {
                        // Not a ChannelReply - dispatch to handle_info for all joined channels
                        let results = session.handle_info_all(crate::RawTerm::from(msg_bytes)).await;
                        for (topic, result) in results {
                            // Handle any broadcasts from handle_info
                            if let RawHandleResult::Broadcast { event, payload } = result {
                                // Broadcast to pg group
                                let group = format!("channel:{}", topic);
                                let msg = ChannelReply::Push {
                                    topic: topic.clone(),
                                    event: event.clone(),
                                    payload: payload.clone(),
                                };
                                if let Ok(bytes) = postcard::to_allocvec(&msg) {
                                    let members = pg::get_members(&group);
                                    for member_pid in members {
                                        let _ = crate::send_raw(member_pid, bytes.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                // Timeout is ok - just means no messages waiting
            }

            // Handle incoming WebSocket messages
            ws_msg = read.next() => {
                let msg = match ws_msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        tracing::warn!(%addr, error = %e, "WebSocket read error");
                        break;
                    }
                    None => break,
                };

                match msg {
                    Message::Text(text) => {
                        // Use the serializer to decode the wire message
                        let decoded = match serializer.decode(text.as_bytes()) {
                            Some(d) => d,
                            None => {
                                tracing::warn!(%addr, "Invalid Phoenix message format");
                                continue;
                            }
                        };

                        // Handle the message using the serializer for encoding replies
                        let replies = handle_decoded_message(&mut session, &serializer, decoded).await;
                        for reply_bytes in replies {
                            if let Err(e) = write.send(Message::Text(String::from_utf8_lossy(&reply_bytes).into_owned().into())).await {
                                tracing::warn!(%addr, error = %e, "WebSocket write error");
                                break;
                            }
                        }
                    }
                    Message::Binary(_) => {
                        // Binary not supported in V2 JSON protocol
                    }
                    Message::Ping(_) => {
                        // tungstenite handles pong automatically
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => {
                        tracing::info!(%addr, "WebSocket closed");
                        break;
                    }
                    Message::Frame(_) => {}
                }
            }
        }
    }

    // Clean up
    session.terminate().await;

    tracing::info!(%addr, "WebSocket disconnected");
    Ok(())
}

/// Handle a decoded Phoenix protocol message using the serializer for encoding replies.
async fn handle_decoded_message(
    session: &mut WsSession,
    serializer: &Arc<dyn Serializer>,
    msg: DecodedMessage,
) -> Vec<Vec<u8>> {
    match msg.event.as_str() {
        "heartbeat" if msg.topic == "phoenix" => {
            // Reply to heartbeat
            vec![serializer.encode_reply(None, msg.msg_ref.as_deref(), "phoenix", "ok", b"{}")]
        }

        "phx_join" => handle_join_decoded(session, serializer, msg).await,

        "phx_leave" => handle_leave_decoded(session, serializer, msg).await,

        // Custom events for joined channels
        _ => handle_channel_event_decoded(session, serializer, msg).await,
    }
}

/// Handle phx_join event with serializer.
async fn handle_join_decoded(
    session: &mut WsSession,
    serializer: &Arc<dyn Serializer>,
    msg: DecodedMessage,
) -> Vec<Vec<u8>> {
    match session
        .handle_join(msg.topic.clone(), msg.payload.clone(), msg.join_ref.clone())
        .await
    {
        Ok(reply_payload) => {
            // Reply payload is already in wire format (JSON) from JsonChannelInstance
            let response = reply_payload.unwrap_or_else(|| b"{}".to_vec());
            vec![serializer.encode_reply(
                msg.join_ref.as_deref(),
                msg.msg_ref.as_deref(),
                &msg.topic,
                "ok",
                &response,
            )]
        }
        Err(e) => {
            let error_payload = json!({ "reason": e.reason }).to_string();
            vec![serializer.encode_reply(
                msg.join_ref.as_deref(),
                msg.msg_ref.as_deref(),
                &msg.topic,
                "error",
                error_payload.as_bytes(),
            )]
        }
    }
}

/// Handle phx_leave event with serializer.
async fn handle_leave_decoded(
    session: &mut WsSession,
    serializer: &Arc<dyn Serializer>,
    msg: DecodedMessage,
) -> Vec<Vec<u8>> {
    session.handle_leave(&msg.topic).await;

    vec![serializer.encode_reply(None, msg.msg_ref.as_deref(), &msg.topic, "ok", b"{}")]
}

/// Handle custom channel events with serializer.
async fn handle_channel_event_decoded(
    session: &mut WsSession,
    serializer: &Arc<dyn Serializer>,
    msg: DecodedMessage,
) -> Vec<Vec<u8>> {
    let mut replies = Vec::new();

    // Check if joined
    if !session.is_joined(&msg.topic) {
        let error_payload = json!({ "reason": "not joined" }).to_string();
        return vec![serializer.encode_reply(
            None,
            msg.msg_ref.as_deref(),
            &msg.topic,
            "error",
            error_payload.as_bytes(),
        )];
    }

    let result = session
        .handle_event(&msg.topic, &msg.event, msg.payload.clone())
        .await;

    if let Some(raw_result) = result {
        match raw_result {
            RawHandleResult::NoReply => {
                // Send ok reply for events that don't have explicit replies
                replies.push(serializer.encode_reply(
                    session.get_join_ref(&msg.topic).as_deref(),
                    msg.msg_ref.as_deref(),
                    &msg.topic,
                    "ok",
                    b"{}",
                ));
            }
            RawHandleResult::Reply { status, payload } => {
                let status_str = match status {
                    ReplyStatus::Ok => "ok",
                    ReplyStatus::Error => "error",
                };
                replies.push(serializer.encode_reply(
                    session.get_join_ref(&msg.topic).as_deref(),
                    msg.msg_ref.as_deref(),
                    &msg.topic,
                    status_str,
                    &payload,
                ));
            }
            RawHandleResult::Push { event, payload } => {
                replies.push(serializer.encode_push(
                    session.get_join_ref(&msg.topic).as_deref(),
                    &msg.topic,
                    &event,
                    &payload,
                ));
            }
            RawHandleResult::Broadcast { event, payload } => {
                // Broadcast to pg group
                let group = format!("channel:{}", msg.topic);
                let channel_msg = ChannelReply::Push {
                    topic: msg.topic.clone(),
                    event,
                    payload,
                };
                if let Ok(bytes) = postcard::to_allocvec(&channel_msg) {
                    let members = pg::get_members(&group);
                    for member_pid in members {
                        let _ = crate::send_raw(member_pid, bytes.clone());
                    }
                }
                // Send ok reply
                replies.push(serializer.encode_reply(
                    session.get_join_ref(&msg.topic).as_deref(),
                    msg.msg_ref.as_deref(),
                    &msg.topic,
                    "ok",
                    b"{}",
                ));
            }
            RawHandleResult::BroadcastFrom { event, payload } => {
                // Broadcast to pg group except sender
                let group = format!("channel:{}", msg.topic);
                let channel_msg = ChannelReply::Push {
                    topic: msg.topic.clone(),
                    event,
                    payload,
                };
                if let Ok(bytes) = postcard::to_allocvec(&channel_msg) {
                    let members = pg::get_members(&group);
                    for member_pid in members {
                        if member_pid != session.pid {
                            let _ = crate::send_raw(member_pid, bytes.clone());
                        }
                    }
                }
                // Send ok reply
                replies.push(serializer.encode_reply(
                    session.get_join_ref(&msg.topic).as_deref(),
                    msg.msg_ref.as_deref(),
                    &msg.topic,
                    "ok",
                    b"{}",
                ));
            }
            RawHandleResult::Stop { reason } => {
                session.handle_leave(&msg.topic).await;
                tracing::debug!(reason = %reason, "Channel stopped");
            }
        }
    } else {
        // No result - not joined or handler not found
        let error_payload = json!({ "reason": "handler error" }).to_string();
        replies.push(serializer.encode_reply(
            session.get_join_ref(&msg.topic).as_deref(),
            msg.msg_ref.as_deref(),
            &msg.topic,
            "error",
            error_payload.as_bytes(),
        ));
    }

    replies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phx_message_parse() {
        let json = json!(["join-1", "ref-1", "room:lobby", "phx_join", {"nick": "alice"}]);
        let msg = PhxMessage::from_json(&json).unwrap();

        assert_eq!(msg.join_ref, Some("join-1".to_string()));
        assert_eq!(msg.msg_ref, Some("ref-1".to_string()));
        assert_eq!(msg.topic, "room:lobby");
        assert_eq!(msg.event, "phx_join");
        assert_eq!(msg.payload["nick"], "alice");
    }

    #[test]
    fn test_phx_message_serialize() {
        let msg = PhxMessage::new("room:lobby", "new_msg", json!({"text": "hello"}))
            .with_join_ref("join-1")
            .with_msg_ref("ref-1");

        let json = msg.to_json();
        let arr = json.as_array().unwrap();

        assert_eq!(arr[0], "join-1");
        assert_eq!(arr[1], "ref-1");
        assert_eq!(arr[2], "room:lobby");
        assert_eq!(arr[3], "new_msg");
        assert_eq!(arr[4]["text"], "hello");
    }

    #[test]
    fn test_phx_message_reply() {
        let msg = PhxMessage::ok_reply(
            Some("join-1".to_string()),
            Some("ref-1".to_string()),
            "room:lobby",
            json!({"users": ["alice", "bob"]}),
        );

        assert_eq!(msg.event, "phx_reply");
        assert_eq!(msg.payload["status"], "ok");
        assert_eq!(msg.payload["response"]["users"][0], "alice");
    }

    #[test]
    fn test_phx_message_error_reply() {
        let msg = PhxMessage::error_reply(
            None,
            Some("ref-1".to_string()),
            "room:lobby",
            "not authorized",
        );

        assert_eq!(msg.event, "phx_reply");
        assert_eq!(msg.payload["status"], "error");
        assert_eq!(msg.payload["response"]["reason"], "not authorized");
    }

    #[test]
    fn test_invalid_message_format() {
        // Wrong number of elements
        let json = json!(["join-1", "ref-1", "room:lobby", "phx_join"]);
        assert!(PhxMessage::from_json(&json).is_none());

        // Not an array
        let json = json!({"topic": "room:lobby"});
        assert!(PhxMessage::from_json(&json).is_none());
    }
}
