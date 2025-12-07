//! Core channel types and traits.
//!
//! Channels provide Phoenix-style real-time communication with:
//! - `&mut self` style handlers (struct IS the channel state)
//! - Enum-based message dispatch (like GenServer)
//! - Topic-based routing with wildcard patterns

use crate::core::{Pid, RawTerm};
use crate::dist::pg;
use crate::message::Message as MessageTrait;
pub use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Socket - Connection metadata
// ============================================================================

/// Connection metadata passed to handlers.
///
/// In the new API, Socket is just connection info. The channel struct
/// itself holds the state.
#[derive(Debug, Clone)]
pub struct Socket {
    /// The process ID of the connection handler.
    pub pid: Pid,
    /// The topic this socket is connected to.
    pub topic: String,
    /// The unique join reference for this connection.
    pub join_ref: String,
}

impl Socket {
    /// Create a new socket with the given PID and topic.
    pub fn new(pid: Pid, topic: impl Into<String>) -> Self {
        Socket {
            pid,
            topic: topic.into(),
            join_ref: uuid_simple(),
        }
    }
}

// ============================================================================
// Phoenix-style Channel Helper Functions
// ============================================================================

/// Push a message directly to a socket's client.
///
/// This mirrors Phoenix's `push/3` - sends an event directly to one client
/// without requiring a prior message from the client.
pub fn push<T: Serialize>(socket: &Socket, event: &str, payload: &T) {
    if let Ok(payload_bytes) = postcard::to_allocvec(payload) {
        let msg = ChannelReply::Push {
            topic: socket.topic.clone(),
            event: event.to_string(),
            payload: payload_bytes,
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let _ = crate::send_raw(socket.pid, bytes);
        }
    }
}

/// Broadcast a message to all subscribers of a topic.
///
/// This mirrors Phoenix's `broadcast/3` - sends to all subscribers of the
/// socket's topic, including the sender.
pub fn broadcast<T: Serialize>(socket: &Socket, event: &str, payload: &T) {
    broadcast_to_topic(&socket.topic, event, payload);
}

/// Broadcast a message to all subscribers of a topic, excluding the sender.
///
/// This mirrors Phoenix's `broadcast_from/3` - sends to all subscribers except
/// the socket that initiated the broadcast.
pub fn broadcast_from<T: Serialize>(socket: &Socket, event: &str, payload: &T) {
    if let Ok(payload_bytes) = postcard::to_allocvec(payload) {
        let msg = ChannelReply::Push {
            topic: socket.topic.clone(),
            event: event.to_string(),
            payload: payload_bytes,
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let group = format!("channel:{}", socket.topic);
            let members = pg::get_members(&group);
            tracing::debug!(
                topic = %socket.topic,
                event = %event,
                sender = ?socket.pid,
                member_count = members.len(),
                members = ?members,
                "broadcast_from: sending to pg group members"
            );
            for pid in members {
                if pid != socket.pid {
                    let result = crate::send_raw(pid, bytes.clone());
                    tracing::debug!(
                        to = ?pid,
                        success = result.is_ok(),
                        error = ?result.err(),
                        "broadcast_from: sent to member"
                    );
                }
            }
        }
    }
}

/// Broadcast a message to all subscribers of a specific topic.
pub fn broadcast_to_topic<T: Serialize>(topic: &str, event: &str, payload: &T) {
    if let Ok(payload_bytes) = postcard::to_allocvec(payload) {
        let msg = ChannelReply::Push {
            topic: topic.to_string(),
            event: event.to_string(),
            payload: payload_bytes,
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let group = format!("channel:{}", topic);
            let members = pg::get_members(&group);
            for pid in members {
                let _ = crate::send_raw(pid, bytes.clone());
            }
        }
    }
}

/// Broadcast a message to all subscribers of a topic except a specific PID.
pub fn broadcast_to_topic_from<T: Serialize>(topic: &str, from_pid: Pid, event: &str, payload: &T) {
    if let Ok(payload_bytes) = postcard::to_allocvec(payload) {
        let msg = ChannelReply::Push {
            topic: topic.to_string(),
            event: event.to_string(),
            payload: payload_bytes,
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let group = format!("channel:{}", topic);
            let members = pg::get_members(&group);
            for pid in members {
                if pid != from_pid {
                    let _ = crate::send_raw(pid, bytes.clone());
                }
            }
        }
    }
}

// ============================================================================
// Result Types
// ============================================================================

/// Result of a channel join attempt.
#[derive(Debug)]
pub enum JoinResult<C> {
    /// Join succeeded - returns the constructed channel.
    Ok(C),
    /// Join succeeded with a reply payload to send back.
    OkReply(C, Vec<u8>),
    /// Join was denied.
    Error(JoinError),
}

/// Error returned when a join is denied.
#[derive(Debug, Clone)]
pub struct JoinError {
    /// The reason for denial.
    pub reason: String,
    /// Optional additional data.
    pub details: Option<HashMap<String, String>>,
}

impl JoinError {
    /// Create a new join error with a reason.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            details: None,
        }
    }

    /// Add details to the error.
    pub fn with_details(mut self, details: HashMap<String, String>) -> Self {
        self.details = Some(details);
        self
    }
}

/// Result of handling an incoming message.
#[derive(Debug)]
pub enum HandleResult<O = ()> {
    /// No reply needed.
    NoReply,
    /// Send a typed reply to the client.
    Reply {
        /// Status of the reply (ok, error).
        status: ReplyStatus,
        /// Response payload.
        payload: O,
    },
    /// Send a raw reply to the client (already serialized).
    ReplyRaw {
        /// Status of the reply (ok, error).
        status: ReplyStatus,
        /// Pre-serialized response payload.
        payload: Vec<u8>,
    },
    /// Broadcast to all subscribers on the topic.
    Broadcast {
        /// Event name.
        event: String,
        /// Broadcast payload.
        payload: O,
    },
    /// Broadcast to all subscribers except the sender.
    BroadcastFrom {
        /// Event name.
        event: String,
        /// Broadcast payload.
        payload: O,
    },
    /// Push a message to just this client.
    Push {
        /// Event name.
        event: String,
        /// Push payload.
        payload: O,
    },
    /// Push a raw message to just this client (already serialized).
    PushRaw {
        /// Event name.
        event: String,
        /// Pre-serialized payload.
        payload: Vec<u8>,
    },
    /// Stop the channel.
    Stop {
        /// Reason for stopping.
        reason: String,
    },
}

/// Status for a reply message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyStatus {
    /// Successful reply.
    Ok,
    /// Error reply.
    Error,
}

/// Reason for channel termination.
#[derive(Debug, Clone)]
pub enum TerminateReason {
    /// Normal shutdown.
    Normal,
    /// Client left the channel.
    Left,
    /// Connection was closed.
    Closed,
    /// Shutdown with a reason.
    Shutdown(String),
    /// Error occurred.
    Error(String),
}

// ============================================================================
// Channel Trait - The struct IS the state
// ============================================================================

/// Core Channel trait - the struct IS the channel state.
///
/// Implement this trait to create a Channel. The struct's fields
/// are the channel state. `join` constructs the struct, and handlers
/// receive `&mut self` to mutate it.
///
/// # Example
///
/// ```ignore
/// use ambitious::channel::{Channel, Socket, JoinResult, HandleResult, ReplyStatus, async_trait};
/// use ambitious::Message;
/// use serde::{Deserialize, Serialize};
///
/// pub struct LobbyChannel {
///     nick: Option<String>,
/// }
///
/// // Join payload
/// #[derive(Debug, Clone, Serialize, Deserialize, Message)]
/// pub struct LobbyJoin {
///     nick: Option<String>,
/// }
///
/// // All incoming events in one enum
/// #[derive(Debug, Clone, Serialize, Deserialize, Message)]
/// pub enum LobbyIn {
///     ListRooms,
/// }
///
/// // All outgoing events in one enum
/// #[derive(Debug, Clone, Serialize, Deserialize, Message)]
/// pub enum LobbyOut {
///     RoomList { rooms: Vec<String> },
/// }
///
/// #[async_trait]
/// impl Channel for LobbyChannel {
///     type Join = LobbyJoin;
///     type In = LobbyIn;
///     type Out = LobbyOut;
///     type Info = ();
///     const TOPIC_PATTERN: &'static str = "lobby:*";
///
///     async fn join(_topic: &str, payload: LobbyJoin, _socket: &Socket) -> JoinResult<Self> {
///         JoinResult::Ok(Self { nick: payload.nick })
///     }
///
///     async fn handle_in(&mut self, event: &str, msg: LobbyIn, _socket: &Socket) -> HandleResult<LobbyOut> {
///         match msg {
///             LobbyIn::ListRooms => HandleResult::Reply {
///                 status: ReplyStatus::Ok,
///                 payload: LobbyOut::RoomList { rooms: vec!["lobby".into()] },
///             },
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait Channel: Sized + Send + Sync + 'static {
    /// The join payload type.
    type Join: MessageTrait;

    /// Enum of all incoming event types.
    type In: MessageTrait;

    /// Enum of all outgoing event/reply types.
    type Out: Serialize + Send + 'static;

    /// Enum of info message types (messages from other processes).
    type Info: MessageTrait;

    /// The topic pattern this channel handles (e.g., "room:*").
    ///
    /// Use `*` as a wildcard to match any suffix.
    const TOPIC_PATTERN: &'static str;

    /// Called when a client attempts to join a topic.
    ///
    /// This constructs the channel state. Return `JoinResult::Ok(self)`
    /// to allow the join, or `JoinResult::Error` to deny.
    async fn join(topic: &str, payload: Self::Join, socket: &Socket) -> JoinResult<Self>;

    /// Called when the channel is terminating.
    ///
    /// Override this for cleanup logic.
    async fn terminate(&mut self, _reason: TerminateReason, _socket: &Socket) {}

    /// Handle an incoming event from a client.
    ///
    /// The `event` parameter is the event name string from the client.
    /// The `msg` parameter is the decoded message.
    async fn handle_in(
        &mut self,
        event: &str,
        msg: Self::In,
        socket: &Socket,
    ) -> HandleResult<Self::Out>;

    /// Handle an info message from another process.
    ///
    /// Override this to handle messages sent to the channel's process.
    async fn handle_info(&mut self, _msg: Self::Info, _socket: &Socket) -> HandleResult<Self::Out> {
        HandleResult::NoReply
    }

    /// Handle a raw incoming event (for transport layer).
    ///
    /// Default implementation decodes the message and calls `handle_in`.
    async fn handle_in_raw(
        &mut self,
        event: &str,
        payload: &[u8],
        socket: &Socket,
    ) -> RawHandleResult {
        match Self::In::decode_local(payload) {
            Ok(msg) => {
                let result = self.handle_in(event, msg, socket).await;
                handle_result_to_raw(result)
            }
            Err(e) => {
                tracing::warn!(event = %event, error = %e, "Failed to decode channel message");
                RawHandleResult::NoReply
            }
        }
    }

    /// Handle a raw info message (for transport layer).
    ///
    /// Default implementation decodes the message and calls `handle_info`.
    async fn handle_info_raw(&mut self, msg: RawTerm, socket: &Socket) -> RawHandleResult {
        match Self::Info::decode_local(msg.as_bytes()) {
            Ok(info) => {
                let result = self.handle_info(info, socket).await;
                handle_result_to_raw(result)
            }
            Err(_) => RawHandleResult::NoReply,
        }
    }
}

// ============================================================================
// Raw Result Types (for dispatch)
// ============================================================================

/// Raw result from handle_in_raw or handle_info_raw.
#[derive(Debug)]
pub enum RawHandleResult {
    /// No reply needed.
    NoReply,
    /// Send a reply (already serialized).
    Reply {
        /// Status of the reply.
        status: ReplyStatus,
        /// Serialized payload.
        payload: Vec<u8>,
    },
    /// Broadcast to all subscribers.
    Broadcast {
        /// Event name.
        event: String,
        /// Serialized payload.
        payload: Vec<u8>,
    },
    /// Broadcast to all except sender.
    BroadcastFrom {
        /// Event name.
        event: String,
        /// Serialized payload.
        payload: Vec<u8>,
    },
    /// Push to this client only.
    Push {
        /// Event name.
        event: String,
        /// Serialized payload.
        payload: Vec<u8>,
    },
    /// Stop the channel.
    Stop {
        /// Reason for stopping.
        reason: String,
    },
}

/// Convert a typed HandleResult to RawHandleResult.
pub fn handle_result_to_raw<T: Serialize>(result: HandleResult<T>) -> RawHandleResult {
    match result {
        HandleResult::NoReply => RawHandleResult::NoReply,
        HandleResult::Reply { status, payload } => match postcard::to_allocvec(&payload) {
            Ok(bytes) => RawHandleResult::Reply {
                status,
                payload: bytes,
            },
            Err(_) => RawHandleResult::NoReply,
        },
        HandleResult::ReplyRaw { status, payload } => RawHandleResult::Reply { status, payload },
        HandleResult::Broadcast { event, payload } => match postcard::to_allocvec(&payload) {
            Ok(bytes) => RawHandleResult::Broadcast {
                event,
                payload: bytes,
            },
            Err(_) => RawHandleResult::NoReply,
        },
        HandleResult::BroadcastFrom { event, payload } => match postcard::to_allocvec(&payload) {
            Ok(bytes) => RawHandleResult::BroadcastFrom {
                event,
                payload: bytes,
            },
            Err(_) => RawHandleResult::NoReply,
        },
        HandleResult::Push { event, payload } => match postcard::to_allocvec(&payload) {
            Ok(bytes) => RawHandleResult::Push {
                event,
                payload: bytes,
            },
            Err(_) => RawHandleResult::NoReply,
        },
        HandleResult::PushRaw { event, payload } => RawHandleResult::Push { event, payload },
        HandleResult::Stop { reason } => RawHandleResult::Stop { reason },
    }
}

// ============================================================================
// Topic Matching
// ============================================================================

/// Check if a topic matches a pattern.
///
/// Patterns support `*` as a wildcard suffix:
/// - `"room:lobby"` matches only `"room:lobby"`
/// - `"room:*"` matches `"room:lobby"`, `"room:123"`, etc.
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        topic.starts_with(prefix)
    } else {
        pattern == topic
    }
}

/// Extract the wildcard portion of a topic given a pattern.
pub fn topic_wildcard<'a>(pattern: &str, topic: &'a str) -> Option<&'a str> {
    pattern
        .strip_suffix('*')
        .and_then(|prefix| topic.strip_prefix(prefix))
}

/// Generate a simple unique ID.
fn uuid_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("{:x}-{:x}", timestamp, count)
}

// ============================================================================
// Channel Messages
// ============================================================================

/// A channel message (incoming or outgoing).
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    /// The topic this message belongs to.
    pub topic: String,
    /// The event name.
    pub event: String,
    /// The payload (serialized).
    pub payload: Vec<u8>,
    /// Message reference for request/reply correlation.
    #[serde(rename = "ref")]
    pub msg_ref: Option<String>,
    /// Join reference for this channel connection.
    pub join_ref: Option<String>,
}

impl Message {
    /// Create a new message.
    pub fn new(topic: impl Into<String>, event: impl Into<String>, payload: Vec<u8>) -> Self {
        Self {
            topic: topic.into(),
            event: event.into(),
            payload,
            msg_ref: None,
            join_ref: None,
        }
    }

    /// Set the message reference.
    pub fn with_ref(mut self, msg_ref: impl Into<String>) -> Self {
        self.msg_ref = Some(msg_ref.into());
        self
    }

    /// Set the join reference.
    pub fn with_join_ref(mut self, join_ref: impl Into<String>) -> Self {
        self.join_ref = Some(join_ref.into());
        self
    }
}

/// Reply sent back to a client from the channel server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelReply {
    /// Join succeeded.
    JoinOk {
        /// Reference for correlation.
        msg_ref: String,
        /// Optional reply payload.
        payload: Option<Vec<u8>>,
    },
    /// Join failed.
    JoinError {
        /// Reference for correlation.
        msg_ref: String,
        /// Error reason.
        reason: String,
    },
    /// Reply to an event.
    Reply {
        /// Reference for correlation.
        msg_ref: String,
        /// Status (ok or error).
        status: String,
        /// Reply payload.
        payload: Vec<u8>,
    },
    /// Push a message to the client.
    Push {
        /// The topic.
        topic: String,
        /// Event name.
        event: String,
        /// Payload.
        payload: Vec<u8>,
    },
}

// ============================================================================
// Type-erased Channel Handler
// ============================================================================

/// A type-erased channel handler for dynamic dispatch.
pub type DynChannelHandler = Arc<dyn ChannelHandler>;

/// Payload serialization format for transport encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    /// JSON encoding (for WebSocket/HTTP transports).
    Json,
    /// Postcard encoding (for binary transports).
    Postcard,
}

/// Factory function to create a channel instance.
pub type ChannelFactory<C> = Box<
    dyn Fn(
            &str,
            &[u8],
            &Socket,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<C, JoinError>> + Send>>
        + Send
        + Sync,
>;

/// Trait for type-erased channel handling.
///
/// This trait bridges the typed Channel trait with the runtime infrastructure.
#[async_trait]
pub trait ChannelHandler: Send + Sync {
    /// Get the topic pattern this handler matches.
    fn topic_pattern(&self) -> &'static str;

    /// Check if this handler matches a topic.
    fn matches(&self, topic: &str) -> bool;

    /// Handle a join request, returning a boxed channel instance.
    async fn handle_join(
        &self,
        topic: &str,
        payload: &[u8],
        socket: Socket,
    ) -> Result<(Box<dyn ChannelInstance>, Option<Vec<u8>>), JoinError>;
}

/// A running channel instance (type-erased).
#[async_trait]
pub trait ChannelInstance: Send + Sync {
    /// Handle an incoming event.
    async fn handle_event(
        &mut self,
        event: &str,
        payload: &[u8],
        socket: &Socket,
    ) -> RawHandleResult;

    /// Handle an info message.
    async fn handle_info(&mut self, msg: RawTerm, socket: &Socket) -> RawHandleResult;

    /// Handle termination.
    async fn terminate(&mut self, reason: TerminateReason, socket: &Socket);
}

/// Wrapper to make a typed Channel into a type-erased ChannelHandler.
pub struct TypedChannelHandler<C: Channel> {
    _phantom: std::marker::PhantomData<C>,
}

impl<C: Channel> TypedChannelHandler<C> {
    /// Create a new typed channel handler.
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<C: Channel> Default for TypedChannelHandler<C> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<C: Channel> ChannelHandler for TypedChannelHandler<C> {
    fn topic_pattern(&self) -> &'static str {
        C::TOPIC_PATTERN
    }

    fn matches(&self, topic: &str) -> bool {
        topic_matches(C::TOPIC_PATTERN, topic)
    }

    async fn handle_join(
        &self,
        topic: &str,
        payload: &[u8],
        socket: Socket,
    ) -> Result<(Box<dyn ChannelInstance>, Option<Vec<u8>>), JoinError> {
        let join_payload: C::Join = C::Join::decode_local(payload)
            .map_err(|e| JoinError::new(format!("invalid payload: {}", e)))?;

        match C::join(topic, join_payload, &socket).await {
            JoinResult::Ok(channel) => Ok((Box::new(TypedChannelInstance { channel }), None)),
            JoinResult::OkReply(channel, reply) => {
                Ok((Box::new(TypedChannelInstance { channel }), Some(reply)))
            }
            JoinResult::Error(e) => Err(e),
        }
    }
}

/// A typed channel instance wrapper.
struct TypedChannelInstance<C: Channel> {
    channel: C,
}

#[async_trait]
impl<C: Channel> ChannelInstance for TypedChannelInstance<C> {
    async fn handle_event(
        &mut self,
        event: &str,
        payload: &[u8],
        socket: &Socket,
    ) -> RawHandleResult {
        self.channel.handle_in_raw(event, payload, socket).await
    }

    async fn handle_info(&mut self, msg: RawTerm, socket: &Socket) -> RawHandleResult {
        self.channel.handle_info_raw(msg, socket).await
    }

    async fn terminate(&mut self, reason: TerminateReason, socket: &Socket) {
        self.channel.terminate(reason, socket).await;
    }
}

// ============================================================================
// Channel Server
// ============================================================================

/// Messages sent to a ChannelServer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelMessage {
    /// A client wants to join a topic.
    Join {
        /// The topic to join.
        topic: String,
        /// Serialized join payload.
        payload: Vec<u8>,
        /// The client's PID.
        client_pid: Pid,
        /// Reference for reply correlation.
        msg_ref: String,
    },
    /// A client sends an event.
    Event {
        /// The topic.
        topic: String,
        /// Event name.
        event: String,
        /// Serialized event payload.
        payload: Vec<u8>,
        /// Reference for reply correlation.
        msg_ref: String,
    },
    /// A client is leaving a topic.
    Leave {
        /// The topic to leave.
        topic: String,
    },
    /// Broadcast a message to all subscribers of a topic.
    Broadcast {
        /// The topic.
        topic: String,
        /// Event name.
        event: String,
        /// Serialized payload.
        payload: Vec<u8>,
    },
}

/// Active channel connection state.
struct ChannelConnection {
    socket: Socket,
    instance: Box<dyn ChannelInstance>,
}

/// A channel server manages channel connections for a single client.
pub struct ChannelServer {
    /// The client's PID (for sending replies).
    client_pid: Pid,
    /// Registered channel handlers.
    handlers: Vec<DynChannelHandler>,
    /// Active channel connections: topic -> connection.
    channels: HashMap<String, ChannelConnection>,
}

impl ChannelServer {
    /// Create a new channel server for a client.
    pub fn new(client_pid: Pid) -> Self {
        Self {
            client_pid,
            handlers: Vec::new(),
            channels: HashMap::new(),
        }
    }

    /// Register a channel handler.
    pub fn add_handler<C: Channel>(&mut self) {
        self.handlers
            .push(Arc::new(TypedChannelHandler::<C>::new()));
    }

    /// Add a pre-built dynamic channel handler.
    pub fn add_dyn_handler(&mut self, handler: DynChannelHandler) {
        self.handlers.push(handler);
    }

    /// Find a handler for a topic.
    fn find_handler(&self, topic: &str) -> Option<&DynChannelHandler> {
        self.handlers.iter().find(|h| h.matches(topic))
    }

    /// Handle a join request.
    pub async fn handle_join(
        &mut self,
        topic: String,
        payload: Vec<u8>,
        msg_ref: String,
    ) -> ChannelReply {
        // Check if already joined
        if self.channels.contains_key(&topic) {
            return ChannelReply::JoinError {
                msg_ref,
                reason: "already joined".to_string(),
            };
        }

        // Find handler
        let handler = match self.find_handler(&topic) {
            Some(h) => h.clone(),
            None => {
                return ChannelReply::JoinError {
                    msg_ref,
                    reason: format!("no handler for topic: {}", topic),
                };
            }
        };

        // Create socket
        let socket = Socket::new(self.client_pid, &topic);

        // Call join
        match handler.handle_join(&topic, &payload, socket.clone()).await {
            Ok((instance, reply_payload)) => {
                // Subscribe to topic via pg
                let group = format!("channel:{}", topic);
                pg::join(&group, self.client_pid);

                // Store the channel connection
                self.channels
                    .insert(topic, ChannelConnection { socket, instance });

                ChannelReply::JoinOk {
                    msg_ref,
                    payload: reply_payload,
                }
            }
            Err(e) => ChannelReply::JoinError {
                msg_ref,
                reason: e.reason,
            },
        }
    }

    /// Handle an incoming event.
    pub async fn handle_event(
        &mut self,
        topic: String,
        event: String,
        payload: Vec<u8>,
        msg_ref: String,
    ) -> Option<ChannelReply> {
        let conn = self.channels.get_mut(&topic)?;
        let result = conn
            .instance
            .handle_event(&event, &payload, &conn.socket)
            .await;

        match result {
            RawHandleResult::NoReply => None,
            RawHandleResult::Reply { status, payload } => Some(ChannelReply::Reply {
                msg_ref,
                status: match status {
                    ReplyStatus::Ok => "ok".to_string(),
                    ReplyStatus::Error => "error".to_string(),
                },
                payload,
            }),
            RawHandleResult::Broadcast {
                event: broadcast_event,
                payload: broadcast_payload,
            } => {
                self.do_broadcast(&topic, &broadcast_event, broadcast_payload);
                None
            }
            RawHandleResult::BroadcastFrom {
                event: broadcast_event,
                payload: broadcast_payload,
            } => {
                self.do_broadcast_from(&topic, &broadcast_event, broadcast_payload);
                None
            }
            RawHandleResult::Push {
                event: push_event,
                payload: push_payload,
            } => Some(ChannelReply::Push {
                topic,
                event: push_event,
                payload: push_payload,
            }),
            RawHandleResult::Stop { reason } => {
                self.handle_leave(topic).await;
                tracing::debug!(reason = %reason, "Channel stopped");
                None
            }
        }
    }

    /// Handle a leave request.
    pub async fn handle_leave(&mut self, topic: String) {
        if let Some(mut conn) = self.channels.remove(&topic) {
            conn.instance
                .terminate(TerminateReason::Left, &conn.socket)
                .await;

            // Unsubscribe from pg
            let group = format!("channel:{}", topic);
            pg::leave(&group, self.client_pid);
        }
    }

    /// Broadcast to all subscribers of a topic.
    fn do_broadcast(&self, topic: &str, event: &str, payload: Vec<u8>) {
        let group = format!("channel:{}", topic);
        let msg = ChannelReply::Push {
            topic: topic.to_string(),
            event: event.to_string(),
            payload,
        };

        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let members = pg::get_members(&group);
            for pid in members {
                let _ = crate::send_raw(pid, bytes.clone());
            }
        }
    }

    /// Broadcast to all subscribers except the sender.
    fn do_broadcast_from(&self, topic: &str, event: &str, payload: Vec<u8>) {
        let group = format!("channel:{}", topic);
        let msg = ChannelReply::Push {
            topic: topic.to_string(),
            event: event.to_string(),
            payload,
        };

        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let members = pg::get_members(&group);
            for pid in members {
                if pid != self.client_pid {
                    let _ = crate::send_raw(pid, bytes.clone());
                }
            }
        }
    }

    /// Handle termination - clean up all channels.
    pub async fn terminate(&mut self, reason: TerminateReason) {
        let topics: Vec<String> = self.channels.keys().cloned().collect();
        for topic in topics {
            if let Some(mut conn) = self.channels.remove(&topic) {
                conn.instance.terminate(reason.clone(), &conn.socket).await;
                let group = format!("channel:{}", topic);
                pg::leave(&group, self.client_pid);
            }
        }
    }

    /// Get the list of joined topics.
    pub fn joined_topics(&self) -> Vec<&String> {
        self.channels.keys().collect()
    }

    /// Check if joined to a topic.
    pub fn is_joined(&self, topic: &str) -> bool {
        self.channels.contains_key(topic)
    }

    /// Handle an info message for a specific topic.
    pub async fn handle_info(&mut self, topic: &str, msg: RawTerm) -> Option<RawHandleResult> {
        let conn = self.channels.get_mut(topic)?;
        Some(conn.instance.handle_info(msg, &conn.socket).await)
    }

    /// Handle an info message, trying all joined channels.
    pub async fn handle_info_any(&mut self, msg: RawTerm) -> Vec<(String, RawHandleResult)> {
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
}

/// Builder for creating a ChannelServer with registered handlers.
pub struct ChannelServerBuilder {
    handlers: Vec<DynChannelHandler>,
}

impl ChannelServerBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Register a channel handler.
    pub fn channel<C: Channel>(mut self) -> Self {
        self.handlers
            .push(Arc::new(TypedChannelHandler::<C>::new()));
        self
    }

    /// Build the channel server for a client.
    pub fn build(self, client_pid: Pid) -> ChannelServer {
        ChannelServer {
            client_pid,
            handlers: self.handlers,
            channels: HashMap::new(),
        }
    }
}

impl Default for ChannelServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_matches_exact() {
        assert!(topic_matches("room:lobby", "room:lobby"));
        assert!(!topic_matches("room:lobby", "room:other"));
    }

    #[test]
    fn test_topic_matches_wildcard() {
        assert!(topic_matches("room:*", "room:lobby"));
        assert!(topic_matches("room:*", "room:123"));
        assert!(topic_matches("room:*", "room:"));
        assert!(!topic_matches("room:*", "user:123"));
    }

    #[test]
    fn test_topic_wildcard_extraction() {
        assert_eq!(topic_wildcard("room:*", "room:lobby"), Some("lobby"));
        assert_eq!(topic_wildcard("room:*", "room:123"), Some("123"));
        assert_eq!(topic_wildcard("room:lobby", "room:lobby"), None);
    }

    #[test]
    fn test_join_error() {
        let err = JoinError::new("unauthorized");
        assert_eq!(err.reason, "unauthorized");
        assert!(err.details.is_none());

        let mut details = HashMap::new();
        details.insert("code".to_string(), "403".to_string());
        let err = err.with_details(details);
        assert!(err.details.is_some());
    }
}
