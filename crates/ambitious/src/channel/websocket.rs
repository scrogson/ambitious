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
    Channel, ChannelHandler, ChannelReply, ChannelServer, DynChannelHandler, HandleResult,
    JoinError, JoinResult, Socket, TerminateReason,
};
use crate::Pid;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

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
// JSON-based Channel Handler for WebSocket transport
// ============================================================================

/// Wrapper to make a typed Channel into a JSON-based type-erased ChannelHandler.
///
/// Unlike `TypedChannelHandler` which uses postcard (binary), this uses JSON
/// serialization which is required for the Phoenix Channels protocol.
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
    C::Assigns: Serialize + DeserializeOwned,
{
    fn topic_pattern(&self) -> &'static str {
        C::topic_pattern()
    }

    fn matches(&self, topic: &str) -> bool {
        super::topic_matches(C::topic_pattern(), topic)
    }

    async fn handle_join(
        &self,
        topic: &str,
        payload: &[u8],
        socket: Socket<()>,
    ) -> Result<(Socket<Vec<u8>>, Option<Vec<u8>>), JoinError> {
        let join_payload: C::JoinPayload = serde_json::from_slice(payload)
            .map_err(|e| JoinError::new(format!("invalid payload: {}", e)))?;

        match C::join(topic, join_payload, socket).await {
            JoinResult::Ok(typed_socket) => {
                let assigns_bytes = serde_json::to_vec(&typed_socket.assigns)
                    .map_err(|e| JoinError::new(format!("failed to serialize assigns: {}", e)))?;
                let erased_socket = Socket {
                    pid: typed_socket.pid,
                    topic: typed_socket.topic,
                    join_ref: typed_socket.join_ref,
                    assigns: assigns_bytes,
                };
                Ok((erased_socket, None))
            }
            JoinResult::OkReply(typed_socket, reply) => {
                let assigns_bytes = serde_json::to_vec(&typed_socket.assigns)
                    .map_err(|e| JoinError::new(format!("failed to serialize assigns: {}", e)))?;
                let erased_socket = Socket {
                    pid: typed_socket.pid,
                    topic: typed_socket.topic,
                    join_ref: typed_socket.join_ref,
                    assigns: assigns_bytes,
                };
                Ok((erased_socket, Some(reply)))
            }
            JoinResult::Error(e) => Err(e),
        }
    }

    async fn handle_event(
        &self,
        event: &str,
        payload: &[u8],
        socket: &mut Socket<Vec<u8>>,
    ) -> HandleResult<Vec<u8>> {
        let in_event: C::InEvent = match serde_json::from_slice(payload) {
            Ok(e) => e,
            Err(_) => return HandleResult::NoReply,
        };

        let assigns: C::Assigns = match serde_json::from_slice(&socket.assigns) {
            Ok(a) => a,
            Err(_) => return HandleResult::NoReply,
        };

        let mut typed_socket = Socket {
            pid: socket.pid,
            topic: socket.topic.clone(),
            join_ref: socket.join_ref.clone(),
            assigns,
        };

        let result = C::handle_in(event, in_event, &mut typed_socket).await;

        // Update assigns back
        if let Ok(new_assigns) = serde_json::to_vec(&typed_socket.assigns) {
            socket.assigns = new_assigns;
        }

        match result {
            HandleResult::NoReply => HandleResult::NoReply,
            // Typed reply - serialize to JSON for WebSocket client
            HandleResult::Reply { status, payload } => match serde_json::to_vec(&payload) {
                Ok(bytes) => HandleResult::ReplyRaw {
                    status,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::ReplyRaw { status, payload } => {
                HandleResult::ReplyRaw { status, payload }
            }
            // Use postcard for broadcasts so TUI clients (which use postcard) can read them
            HandleResult::Broadcast { event, payload } => match postcard::to_allocvec(&payload) {
                Ok(bytes) => HandleResult::Broadcast {
                    event,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::BroadcastFrom { event, payload } => {
                match postcard::to_allocvec(&payload) {
                    Ok(bytes) => HandleResult::BroadcastFrom {
                        event,
                        payload: bytes,
                    },
                    Err(_) => HandleResult::NoReply,
                }
            }
            // Typed push - serialize to JSON for WebSocket client
            HandleResult::Push { event, payload } => match serde_json::to_vec(&payload) {
                Ok(bytes) => HandleResult::PushRaw {
                    event,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::PushRaw { event, payload } => HandleResult::PushRaw { event, payload },
            HandleResult::Stop { reason } => HandleResult::Stop { reason },
        }
    }

    async fn filter_broadcast(
        &self,
        event: &str,
        payload: &[u8],
        socket: &Socket<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        // Broadcasts use postcard format for consistency with TUI clients
        let out_event: C::OutEvent = postcard::from_bytes(payload).ok()?;
        let assigns: C::Assigns = serde_json::from_slice(&socket.assigns).ok()?;

        let typed_socket = Socket {
            pid: socket.pid,
            topic: socket.topic.clone(),
            join_ref: socket.join_ref.clone(),
            assigns,
        };

        let filtered = C::handle_out(event, out_event, &typed_socket).await?;
        // Return postcard format for distribution
        postcard::to_allocvec(&filtered).ok()
    }

    async fn handle_terminate(&self, reason: TerminateReason, socket: &Socket<Vec<u8>>) {
        if let Ok(assigns) = serde_json::from_slice::<C::Assigns>(&socket.assigns) {
            let typed_socket = Socket {
                pid: socket.pid,
                topic: socket.topic.clone(),
                join_ref: socket.join_ref.clone(),
                assigns,
            };
            C::terminate(reason, &typed_socket).await;
        }
    }

    fn transcode_payload(&self, payload: &[u8], format: super::PayloadFormat) -> Option<Vec<u8>> {
        let out_event: C::OutEvent = postcard::from_bytes(payload).ok()?;
        match format {
            super::PayloadFormat::Json => serde_json::to_vec(&out_event).ok(),
            super::PayloadFormat::Postcard => postcard::to_allocvec(&out_event).ok(),
        }
    }

    async fn handle_info(
        &self,
        msg: crate::core::RawTerm,
        socket: &mut Socket<Vec<u8>>,
    ) -> HandleResult<Vec<u8>> {
        let assigns: C::Assigns = match serde_json::from_slice(&socket.assigns) {
            Ok(a) => a,
            Err(_) => return HandleResult::NoReply,
        };

        let mut typed_socket = Socket {
            pid: socket.pid,
            topic: socket.topic.clone(),
            join_ref: socket.join_ref.clone(),
            assigns,
        };

        let result = C::handle_info(msg, &mut typed_socket).await;

        // Update assigns back
        if let Ok(new_assigns) = serde_json::to_vec(&typed_socket.assigns) {
            socket.assigns = new_assigns;
        }

        match result {
            HandleResult::NoReply => HandleResult::NoReply,
            // Typed reply - serialize to JSON for WebSocket client
            HandleResult::Reply { status, payload } => match serde_json::to_vec(&payload) {
                Ok(bytes) => HandleResult::ReplyRaw {
                    status,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::ReplyRaw { status, payload } => {
                HandleResult::ReplyRaw { status, payload }
            }
            // Use postcard for broadcasts so TUI clients can read them
            HandleResult::Broadcast { event, payload } => match postcard::to_allocvec(&payload) {
                Ok(bytes) => HandleResult::Broadcast {
                    event,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::BroadcastFrom { event, payload } => {
                match postcard::to_allocvec(&payload) {
                    Ok(bytes) => HandleResult::BroadcastFrom {
                        event,
                        payload: bytes,
                    },
                    Err(_) => HandleResult::NoReply,
                }
            }
            // Typed push - serialize to JSON for WebSocket client
            HandleResult::Push { event, payload } => match serde_json::to_vec(&payload) {
                Ok(bytes) => HandleResult::PushRaw {
                    event,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::PushRaw { event, payload } => HandleResult::PushRaw { event, payload },
            HandleResult::Stop { reason } => HandleResult::Stop { reason },
        }
    }
}

/// A WebSocket endpoint that serves Phoenix Channels.
///
/// The endpoint manages WebSocket connections and routes messages to the
/// appropriate channel handlers.
pub struct WebSocketEndpoint {
    handlers: Vec<DynChannelHandler>,
    config: WebSocketConfig,
}

impl WebSocketEndpoint {
    /// Create a new WebSocket endpoint.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            config: WebSocketConfig::default(),
        }
    }

    /// Set the configuration.
    pub fn config(mut self, config: WebSocketConfig) -> Self {
        self.config = config;
        self
    }

    /// Register a channel handler.
    ///
    /// The channel will use JSON serialization for payloads, which is
    /// required for the Phoenix Channels V2 protocol.
    pub fn channel<C>(mut self) -> Self
    where
        C: Channel,
        C::Assigns: Serialize + DeserializeOwned,
    {
        self.handlers.push(Arc::new(JsonChannelHandler::<C>::new()));
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
    pub fn handlers(&self) -> &[DynChannelHandler] {
        &self.handlers
    }
}

impl Default for WebSocketEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// WebSocket session state.
struct WsSession {
    /// Channel server for handling channel operations.
    channels: ChannelServer,
    /// Joined topics with their join_refs.
    joined: HashMap<String, String>,
}

impl WsSession {
    fn new(pid: Pid, handlers: Vec<DynChannelHandler>) -> Self {
        let mut channels = ChannelServer::new(pid);
        for handler in handlers {
            channels.add_dyn_handler(handler);
        }

        Self {
            channels,
            joined: HashMap::new(),
        }
    }
}

/// Handle a WebSocket connection.
async fn handle_connection(
    endpoint: Arc<WebSocketEndpoint>,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(%addr, "WebSocket connection");

    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut write, mut read) = ws_stream.split();

    // Get a PID for this session
    let pid = crate::current_pid();

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
                            // Clone the bytes before moving them
                            let info_results = session.channels.handle_info_any(msg_bytes.clone().into()).await;
                            for (info_topic, result) in info_results {
                                if let HandleResult::Broadcast { event: bc_event, payload: bc_payload } = result {
                                    let group = format!("channel:{}", info_topic);
                                    let msg = ChannelReply::Push {
                                        topic: info_topic.clone(),
                                        event: bc_event,
                                        payload: bc_payload,
                                    };
                                    if let Ok(bytes) = postcard::to_allocvec(&msg) {
                                        let members = crate::dist::pg::get_members(&group);
                                        for member_pid in members {
                                            let _ = crate::send_raw(member_pid, bytes.clone());
                                        }
                                    }
                                }
                            }

                            // Convert the payload from postcard to JSON using the handler
                            // Broadcasts use postcard internally for consistency with TUI clients
                            let json_payload: Value = session.channels
                                .transcode_payload(&topic, &payload, crate::channel::PayloadFormat::Json)
                                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                                .unwrap_or(json!({}));

                            let phx_msg = PhxMessage::push(
                                session.joined.get(&topic).cloned(),
                                topic,
                                event,
                                json_payload,
                            );

                            let json_str = phx_msg.to_json().to_string();
                            if let Err(e) = write.send(Message::Text(json_str.into())).await {
                                tracing::warn!(%addr, error = %e, "WebSocket write error");
                                break;
                            }
                        }
                    } else {
                        // Not a ChannelReply - dispatch to handle_info for all joined channels
                        // Note: Presence messages are now handled by the Presence GenServer
                        // and will be received via PubSub subscriptions
                        let results = session.channels.handle_info_any(msg_bytes.into()).await;
                        for (topic, result) in results {
                            // Handle any broadcasts from handle_info
                            if let HandleResult::Broadcast { event, payload } = result {
                                // Broadcast to pg group
                                let group = format!("channel:{}", topic);
                                let msg = ChannelReply::Push {
                                    topic: topic.clone(),
                                    event: event.clone(),
                                    payload: payload.clone(),
                                };
                                if let Ok(bytes) = postcard::to_allocvec(&msg) {
                                    let members = crate::dist::pg::get_members(&group);
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
                        let value: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!(%addr, error = %e, "Invalid JSON");
                                continue;
                            }
                        };

                        let phx_msg = match PhxMessage::from_json(&value) {
                            Some(m) => m,
                            None => {
                                tracing::warn!(%addr, "Invalid Phoenix message format");
                                continue;
                            }
                        };

                        // Handle the message and send replies directly
                        let replies = handle_phx_message(&mut session, phx_msg).await;
                        for reply in replies {
                            let json_str = reply.to_json().to_string();
                            if let Err(e) = write.send(Message::Text(json_str.into())).await {
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
    session.channels.terminate(TerminateReason::Closed).await;

    tracing::info!(%addr, "WebSocket disconnected");
    Ok(())
}

/// Handle a Phoenix protocol message.
async fn handle_phx_message(session: &mut WsSession, msg: PhxMessage) -> Vec<PhxMessage> {
    match msg.event.as_str() {
        "heartbeat" if msg.topic == "phoenix" => {
            // Reply to heartbeat
            vec![PhxMessage::ok_reply(
                None,
                msg.msg_ref,
                "phoenix",
                json!({}),
            )]
        }

        "phx_join" => handle_join(session, msg).await,

        "phx_leave" => handle_leave(session, msg).await,

        // Custom events for joined channels
        _ => handle_channel_event(session, msg).await,
    }
}

/// Handle phx_join event.
async fn handle_join(session: &mut WsSession, msg: PhxMessage) -> Vec<PhxMessage> {
    let mut replies = Vec::new();

    // Convert JSON payload to bytes for the channel server
    let payload_bytes = match serde_json::to_vec(&msg.payload) {
        Ok(b) => b,
        Err(_) => {
            replies.push(PhxMessage::error_reply(
                msg.join_ref,
                msg.msg_ref,
                msg.topic,
                "invalid payload",
            ));
            return replies;
        }
    };

    let msg_ref = msg.msg_ref.clone().unwrap_or_default();
    let reply = session
        .channels
        .handle_join(msg.topic.clone(), payload_bytes, msg_ref)
        .await;

    match reply {
        ChannelReply::JoinOk { payload, .. } => {
            // Store join_ref for this topic
            if let Some(ref join_ref) = msg.join_ref {
                session.joined.insert(msg.topic.clone(), join_ref.clone());
            }

            // Parse reply payload if present
            let response = payload
                .and_then(|p| serde_json::from_slice(&p).ok())
                .unwrap_or(json!({}));

            replies.push(PhxMessage::ok_reply(
                msg.join_ref,
                msg.msg_ref,
                msg.topic,
                response,
            ));
        }
        ChannelReply::JoinError { reason, .. } => {
            replies.push(PhxMessage::error_reply(
                msg.join_ref,
                msg.msg_ref,
                msg.topic,
                reason,
            ));
        }
        _ => {}
    }

    replies
}

/// Handle phx_leave event.
async fn handle_leave(session: &mut WsSession, msg: PhxMessage) -> Vec<PhxMessage> {
    session.channels.handle_leave(msg.topic.clone()).await;
    session.joined.remove(&msg.topic);

    vec![PhxMessage::ok_reply(
        None,
        msg.msg_ref,
        msg.topic,
        json!({}),
    )]
}

/// Handle custom channel events.
async fn handle_channel_event(session: &mut WsSession, msg: PhxMessage) -> Vec<PhxMessage> {
    let mut replies = Vec::new();

    // Check if joined
    if !session.channels.is_joined(&msg.topic) {
        return vec![PhxMessage::error_reply(
            None,
            msg.msg_ref,
            msg.topic,
            "not joined",
        )];
    }

    // Convert JSON payload to bytes
    let payload_bytes = match serde_json::to_vec(&msg.payload) {
        Ok(b) => b,
        Err(_) => {
            return vec![PhxMessage::error_reply(
                session.joined.get(&msg.topic).cloned(),
                msg.msg_ref,
                msg.topic,
                "invalid payload",
            )];
        }
    };

    let msg_ref = msg.msg_ref.clone().unwrap_or_default();
    let result = session
        .channels
        .handle_event(msg.topic.clone(), msg.event.clone(), payload_bytes, msg_ref)
        .await;

    if let Some(channel_reply) = result {
        match channel_reply {
            ChannelReply::Reply {
                status, payload, ..
            } => {
                let response: Value = serde_json::from_slice(&payload).unwrap_or(json!({}));
                replies.push(PhxMessage::reply(
                    session.joined.get(&msg.topic).cloned(),
                    msg.msg_ref,
                    msg.topic,
                    &status,
                    response,
                ));
            }
            ChannelReply::Push {
                topic,
                event,
                payload,
            } => {
                let response: Value = serde_json::from_slice(&payload).unwrap_or(json!({}));
                replies.push(PhxMessage::push(
                    session.joined.get(&topic).cloned(),
                    topic,
                    event,
                    response,
                ));
            }
            _ => {}
        }
    } else {
        // Send ok reply for events that don't have explicit replies
        replies.push(PhxMessage::ok_reply(
            session.joined.get(&msg.topic).cloned(),
            msg.msg_ref,
            msg.topic,
            json!({}),
        ));
    }

    replies
}

/// JSON serializer/deserializer for channel payloads.
///
/// Use this when you want your channel to work with JSON payloads
/// instead of postcard-serialized bytes.
pub mod json_payload {
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::Value;

    /// Serialize a payload to JSON bytes.
    pub fn serialize<T: Serialize>(payload: &T) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(payload)
    }

    /// Deserialize a payload from JSON bytes.
    pub fn deserialize<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Deserialize a payload from JSON Value.
    pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// Serialize a payload to JSON Value.
    pub fn to_value<T: Serialize>(payload: &T) -> Result<Value, serde_json::Error> {
        serde_json::to_value(payload)
    }
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
