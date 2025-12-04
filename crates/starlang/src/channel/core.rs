//! Core channel types and traits.

use crate::core::{Pid, RawTerm};
use crate::dist::pg;
use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::Arc;

/// A client socket connection with custom assigns.
///
/// Assigns allow you to store arbitrary state associated with a connection,
/// such as user IDs, authentication tokens, or room-specific data.
#[derive(Debug, Clone)]
pub struct Socket<A = ()> {
    /// The process ID of the connection handler.
    pub pid: Pid,
    /// The topic this socket is connected to.
    pub topic: String,
    /// The unique join reference for this connection.
    pub join_ref: String,
    /// Custom state assigned to this socket.
    pub assigns: A,
}

impl<A> Socket<A> {
    /// Create a new socket with the given PID and topic.
    pub fn new(pid: Pid, topic: impl Into<String>) -> Socket<()> {
        Socket {
            pid,
            topic: topic.into(),
            join_ref: uuid_simple(),
            assigns: (),
        }
    }

    /// Assign custom state to this socket.
    pub fn assign<B>(self, assigns: B) -> Socket<B> {
        Socket {
            pid: self.pid,
            topic: self.topic,
            join_ref: self.join_ref,
            assigns,
        }
    }

    /// Update assigns using a function.
    pub fn update_assigns<F>(&mut self, f: F)
    where
        F: FnOnce(&mut A),
    {
        f(&mut self.assigns);
    }
}

impl Socket<()> {
    /// Create a socket with default (unit) assigns.
    pub fn with_pid(pid: Pid, topic: impl Into<String>) -> Self {
        Self::new(pid, topic)
    }
}

// ============================================================================
// Phoenix-style Channel Helper Functions
// ============================================================================

/// Push a message directly to a socket's client.
///
/// This mirrors Phoenix's `push/3` - sends an event directly to one client
/// without requiring a prior message from the client.
///
/// # Example
///
/// ```ignore
/// use starlang::channel::push;
///
/// // In handle_info or handle_in:
/// push(&socket, "new_notification", &NotificationEvent { message: "Hello!" });
/// ```
pub fn push<A, T: Serialize>(socket: &Socket<A>, event: &str, payload: &T) {
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
///
/// # Example
///
/// ```ignore
/// use starlang::channel::broadcast;
///
/// // Broadcast to everyone in the room including sender:
/// broadcast(&socket, "new_msg", &MessageEvent { from: "alice", text: "hello" });
/// ```
pub fn broadcast<A, T: Serialize>(socket: &Socket<A>, event: &str, payload: &T) {
    broadcast_to_topic(&socket.topic, event, payload);
}

/// Broadcast a message to all subscribers of a topic, excluding the sender.
///
/// This mirrors Phoenix's `broadcast_from/3` - sends to all subscribers except
/// the socket that initiated the broadcast.
///
/// # Example
///
/// ```ignore
/// use starlang::channel::broadcast_from;
///
/// // Broadcast to everyone except the sender:
/// broadcast_from(&socket, "user_joined", &JoinEvent { nick: "alice" });
/// ```
pub fn broadcast_from<A, T: Serialize>(socket: &Socket<A>, event: &str, payload: &T) {
    if let Ok(payload_bytes) = postcard::to_allocvec(payload) {
        let msg = ChannelReply::Push {
            topic: socket.topic.clone(),
            event: event.to_string(),
            payload: payload_bytes,
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            let group = format!("channel:{}", socket.topic);
            let members = pg::get_members(&group);
            for pid in members {
                if pid != socket.pid {
                    let _ = crate::send_raw(pid, bytes.clone());
                }
            }
        }
    }
}

/// Broadcast a message to all subscribers of a specific topic.
///
/// Unlike `broadcast/3`, this takes a topic string directly instead of a socket,
/// useful when you need to broadcast without having a socket reference.
///
/// # Example
///
/// ```ignore
/// use starlang::channel::broadcast_to_topic;
///
/// broadcast_to_topic("room:lobby", "system_msg", &SystemEvent { message: "Server restarting" });
/// ```
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
///
/// This is useful when you want to exclude a specific process from the broadcast
/// without having a full socket reference.
///
/// # Example
///
/// ```ignore
/// use starlang::channel::broadcast_to_topic_from;
///
/// // Broadcast to all except the sender PID:
/// broadcast_to_topic_from("room:lobby", sender_pid, "user_left", &LeaveEvent { nick: "bob" });
/// ```
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

/// Result of a channel join attempt.
#[derive(Debug)]
pub enum JoinResult<A> {
    /// Join succeeded.
    Ok(Socket<A>),
    /// Join succeeded with a reply payload to send back.
    OkReply(Socket<A>, Vec<u8>),
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
    /// Send a typed reply to the client (serialized by transport).
    Reply {
        /// Status of the reply (ok, error).
        status: ReplyStatus,
        /// Response payload (will be serialized by transport).
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
        /// Push payload (will be serialized by transport).
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

/// The Channel trait defines how a topic handler processes messages.
///
/// Implement this trait to create custom channel handlers for different
/// topic patterns.
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// Custom state stored in the socket's assigns.
    type Assigns: Clone + Send + Sync + 'static;

    /// The payload type for join requests.
    type JoinPayload: DeserializeOwned + Send;

    /// The payload type for incoming events.
    type InEvent: DeserializeOwned + Send;

    /// The payload type for outgoing broadcasts.
    type OutEvent: Serialize + DeserializeOwned + Send;

    /// The topic pattern this channel handles (e.g., "room:*").
    ///
    /// Use `*` as a wildcard to match any suffix.
    fn topic_pattern() -> &'static str;

    /// Called when a client attempts to join a topic.
    ///
    /// Return `JoinResult::Ok` to allow the join, or `JoinResult::Error` to deny.
    async fn join(
        topic: &str,
        payload: Self::JoinPayload,
        socket: Socket<()>,
    ) -> JoinResult<Self::Assigns>;

    /// Called when a client sends an event to the channel.
    ///
    /// Return a `HandleResult` to specify how to respond.
    async fn handle_in(
        event: &str,
        payload: Self::InEvent,
        socket: &mut Socket<Self::Assigns>,
    ) -> HandleResult<Self::OutEvent>;

    /// Called when a broadcast is about to be sent to a client.
    ///
    /// Override this to filter or transform outgoing broadcasts per-client.
    /// By default, all broadcasts are forwarded as-is.
    ///
    /// Return `None` to suppress the broadcast to this client.
    async fn handle_out(
        _event: &str,
        payload: Self::OutEvent,
        _socket: &Socket<Self::Assigns>,
    ) -> Option<Self::OutEvent> {
        Some(payload)
    }

    /// Called when the channel receives a raw message (e.g., from other processes).
    ///
    /// Override this to handle inter-process communication. Use `msg.decode::<T>()`
    /// to attempt decoding the message into a specific type.
    ///
    /// # Example
    ///
    /// ```ignore
    /// async fn handle_info(
    ///     msg: RawTerm,
    ///     socket: &mut Socket<Self::Assigns>,
    /// ) -> HandleResult<Self::OutEvent> {
    ///     if let Some(tick) = msg.decode::<Tick>() {
    ///         // handle tick message
    ///     } else if let Some(refresh) = msg.decode::<Refresh>() {
    ///         // handle refresh message
    ///     }
    ///     HandleResult::NoReply
    /// }
    /// ```
    async fn handle_info(
        _msg: RawTerm,
        _socket: &mut Socket<Self::Assigns>,
    ) -> HandleResult<Self::OutEvent> {
        HandleResult::NoReply
    }

    /// Called when the channel is terminating.
    ///
    /// Override this for cleanup logic.
    async fn terminate(_reason: TerminateReason, _socket: &Socket<Self::Assigns>) {}
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
///
/// # Example
///
/// ```ignore
/// assert_eq!(topic_wildcard("room:*", "room:lobby"), Some("lobby"));
/// assert_eq!(topic_wildcard("room:lobby", "room:lobby"), None);
/// ```
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

/// A type-erased channel handler for dynamic dispatch.
pub type DynChannelHandler = Arc<dyn ChannelHandler>;

/// Trait for type-erased channel handling.
#[async_trait]
pub trait ChannelHandler: Send + Sync {
    /// Get the topic pattern this handler matches.
    fn topic_pattern(&self) -> &'static str;

    /// Check if this handler matches a topic.
    fn matches(&self, topic: &str) -> bool;

    /// Handle a join request.
    async fn handle_join(
        &self,
        topic: &str,
        payload: &[u8],
        socket: Socket<()>,
    ) -> Result<(Socket<Vec<u8>>, Option<Vec<u8>>), JoinError>;

    /// Handle an incoming event.
    async fn handle_event(
        &self,
        event: &str,
        payload: &[u8],
        socket: &mut Socket<Vec<u8>>,
    ) -> HandleResult<Vec<u8>>;

    /// Handle an outgoing broadcast.
    async fn filter_broadcast(
        &self,
        event: &str,
        payload: &[u8],
        socket: &Socket<Vec<u8>>,
    ) -> Option<Vec<u8>>;

    /// Handle termination.
    async fn handle_terminate(&self, reason: TerminateReason, socket: &Socket<Vec<u8>>);

    /// Handle an info message (raw term from mailbox).
    async fn handle_info(
        &self,
        msg: RawTerm,
        socket: &mut Socket<Vec<u8>>,
    ) -> HandleResult<Vec<u8>>;

    /// Decode a postcard-encoded payload and re-encode for the given format.
    ///
    /// This is used by transports to convert broadcast payloads from the
    /// internal postcard format to their wire format.
    fn transcode_payload(&self, payload: &[u8], format: PayloadFormat) -> Option<Vec<u8>>;
}

/// Payload serialization format for transport encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    /// JSON encoding (for WebSocket/HTTP transports).
    Json,
    /// Postcard encoding (for binary transports).
    Postcard,
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
impl<C: Channel> ChannelHandler for TypedChannelHandler<C>
where
    C::Assigns: Serialize + DeserializeOwned,
{
    fn topic_pattern(&self) -> &'static str {
        C::topic_pattern()
    }

    fn matches(&self, topic: &str) -> bool {
        topic_matches(C::topic_pattern(), topic)
    }

    async fn handle_join(
        &self,
        topic: &str,
        payload: &[u8],
        socket: Socket<()>,
    ) -> Result<(Socket<Vec<u8>>, Option<Vec<u8>>), JoinError> {
        let join_payload: C::JoinPayload = postcard::from_bytes(payload)
            .map_err(|e| JoinError::new(format!("invalid payload: {}", e)))?;

        match C::join(topic, join_payload, socket).await {
            JoinResult::Ok(typed_socket) => {
                let assigns_bytes = postcard::to_allocvec(&typed_socket.assigns)
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
                let assigns_bytes = postcard::to_allocvec(&typed_socket.assigns)
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
        let in_event: C::InEvent = match postcard::from_bytes(payload) {
            Ok(e) => e,
            Err(_) => return HandleResult::NoReply,
        };

        let assigns: C::Assigns = match postcard::from_bytes(&socket.assigns) {
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
        if let Ok(new_assigns) = postcard::to_allocvec(&typed_socket.assigns) {
            socket.assigns = new_assigns;
        }

        match result {
            HandleResult::NoReply => HandleResult::NoReply,
            HandleResult::Reply { status, payload } => match postcard::to_allocvec(&payload) {
                Ok(bytes) => HandleResult::ReplyRaw {
                    status,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::ReplyRaw { status, payload } => {
                HandleResult::ReplyRaw { status, payload }
            }
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
            HandleResult::Push { event, payload } => match postcard::to_allocvec(&payload) {
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
        let out_event: C::OutEvent = postcard::from_bytes(payload).ok()?;
        let assigns: C::Assigns = postcard::from_bytes(&socket.assigns).ok()?;

        let typed_socket = Socket {
            pid: socket.pid,
            topic: socket.topic.clone(),
            join_ref: socket.join_ref.clone(),
            assigns,
        };

        let filtered = C::handle_out(event, out_event, &typed_socket).await?;
        postcard::to_allocvec(&filtered).ok()
    }

    async fn handle_terminate(&self, reason: TerminateReason, socket: &Socket<Vec<u8>>) {
        if let Ok(assigns) = postcard::from_bytes::<C::Assigns>(&socket.assigns) {
            let typed_socket = Socket {
                pid: socket.pid,
                topic: socket.topic.clone(),
                join_ref: socket.join_ref.clone(),
                assigns,
            };
            C::terminate(reason, &typed_socket).await;
        }
    }

    fn transcode_payload(&self, payload: &[u8], format: PayloadFormat) -> Option<Vec<u8>> {
        let out_event: C::OutEvent = postcard::from_bytes(payload).ok()?;
        match format {
            PayloadFormat::Postcard => postcard::to_allocvec(&out_event).ok(),
            #[cfg(feature = "websocket")]
            PayloadFormat::Json => serde_json::to_vec(&out_event).ok(),
            #[cfg(not(feature = "websocket"))]
            PayloadFormat::Json => None,
        }
    }

    async fn handle_info(
        &self,
        msg: RawTerm,
        socket: &mut Socket<Vec<u8>>,
    ) -> HandleResult<Vec<u8>> {
        let assigns: C::Assigns = match postcard::from_bytes(&socket.assigns) {
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
        if let Ok(new_assigns) = postcard::to_allocvec(&typed_socket.assigns) {
            socket.assigns = new_assigns;
        }

        match result {
            HandleResult::NoReply => HandleResult::NoReply,
            HandleResult::Reply { status, payload } => match postcard::to_allocvec(&payload) {
                Ok(bytes) => HandleResult::ReplyRaw {
                    status,
                    payload: bytes,
                },
                Err(_) => HandleResult::NoReply,
            },
            HandleResult::ReplyRaw { status, payload } => {
                HandleResult::ReplyRaw { status, payload }
            }
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
            HandleResult::Push { event, payload } => match postcard::to_allocvec(&payload) {
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

// ============================================================================
// Channel Server - Runtime for managing channel connections
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

/// A channel server manages channel connections for a single client.
///
/// Each client connection spawns a ChannelServer process that:
/// - Handles join/leave requests
/// - Routes incoming events to the appropriate channel handler
/// - Manages subscriptions via PubSub
/// - Forwards broadcasts to the client
pub struct ChannelServer {
    /// The client's PID (for sending replies).
    client_pid: Pid,
    /// Registered channel handlers.
    handlers: Vec<DynChannelHandler>,
    /// Active channel connections: topic -> socket.
    channels: HashMap<String, Socket<Vec<u8>>>,
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
    pub fn add_handler<C: Channel>(&mut self)
    where
        C::Assigns: Serialize + DeserializeOwned,
    {
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

        // Create initial socket
        let socket = Socket::with_pid(self.client_pid, &topic);

        // Call join
        match handler.handle_join(&topic, &payload, socket).await {
            Ok((socket, reply_payload)) => {
                // Subscribe to topic via pg
                let group = format!("channel:{}", topic);
                pg::join(&group, self.client_pid);

                // Store the channel connection
                self.channels.insert(topic, socket);

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
        // Find handler first (immutable borrow)
        let handler = self.find_handler(&topic)?.clone();

        // Get the channel socket (mutable borrow)
        let socket = self.channels.get_mut(&topic)?;

        // Handle the event
        let result = handler.handle_event(&event, &payload, socket).await;

        match result {
            HandleResult::NoReply => None,
            HandleResult::Reply { status, payload } => Some(ChannelReply::Reply {
                msg_ref,
                status: match status {
                    ReplyStatus::Ok => "ok".to_string(),
                    ReplyStatus::Error => "error".to_string(),
                },
                payload,
            }),
            HandleResult::ReplyRaw { status, payload } => Some(ChannelReply::Reply {
                msg_ref,
                status: match status {
                    ReplyStatus::Ok => "ok".to_string(),
                    ReplyStatus::Error => "error".to_string(),
                },
                payload,
            }),
            HandleResult::Broadcast {
                event: broadcast_event,
                payload: broadcast_payload,
            } => {
                // Broadcast to all subscribers
                self.do_broadcast(&topic, &broadcast_event, broadcast_payload);
                None
            }
            HandleResult::BroadcastFrom {
                event: broadcast_event,
                payload: broadcast_payload,
            } => {
                // Broadcast to all except sender
                self.do_broadcast_from(&topic, &broadcast_event, broadcast_payload);
                None
            }
            HandleResult::Push {
                event: push_event,
                payload: push_payload,
            } => Some(ChannelReply::Push {
                topic,
                event: push_event,
                payload: push_payload,
            }),
            HandleResult::PushRaw {
                event: push_event,
                payload: push_payload,
            } => Some(ChannelReply::Push {
                topic,
                event: push_event,
                payload: push_payload,
            }),
            HandleResult::Stop { reason } => {
                // Leave the channel
                self.handle_leave(topic).await;
                tracing::debug!(reason = %reason, "Channel stopped");
                None
            }
        }
    }

    /// Handle a leave request.
    pub async fn handle_leave(&mut self, topic: String) {
        if let Some(socket) = self.channels.remove(&topic) {
            // Find handler for terminate callback
            if let Some(handler) = self.find_handler(&topic) {
                handler
                    .handle_terminate(TerminateReason::Left, &socket)
                    .await;
            }

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
            if let Some(socket) = self.channels.remove(&topic) {
                if let Some(handler) = self.find_handler(&topic) {
                    handler.handle_terminate(reason.clone(), &socket).await;
                }
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

    /// Transcode a postcard-encoded payload to another format.
    ///
    /// This finds the appropriate handler for the topic and uses it to convert
    /// the payload from postcard to the requested format.
    pub fn transcode_payload(
        &self,
        topic: &str,
        payload: &[u8],
        format: PayloadFormat,
    ) -> Option<Vec<u8>> {
        let handler = self.find_handler(topic)?;
        handler.transcode_payload(payload, format)
    }

    /// Handle an info message for a specific topic.
    ///
    /// This dispatches the raw message to the channel's handle_info callback.
    pub async fn handle_info(
        &mut self,
        topic: &str,
        msg: RawTerm,
    ) -> Option<HandleResult<Vec<u8>>> {
        let handler = self.find_handler(topic)?.clone();
        let socket = self.channels.get_mut(topic)?;
        Some(handler.handle_info(msg, socket).await)
    }

    /// Handle an info message, trying all joined channels.
    ///
    /// This is used when we receive a message but don't know which channel it's for.
    pub async fn handle_info_any(&mut self, msg: RawTerm) -> Vec<(String, HandleResult<Vec<u8>>)> {
        let mut results = Vec::new();
        let topics: Vec<String> = self.channels.keys().cloned().collect();

        for topic in topics {
            if let Some(handler) = self.find_handler(&topic).cloned()
                && let Some(socket) = self.channels.get_mut(&topic) {
                    let result = handler.handle_info(msg.clone(), socket).await;
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
    pub fn channel<C: Channel>(mut self) -> Self
    where
        C::Assigns: Serialize + DeserializeOwned,
    {
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
    fn test_socket_assign() {
        let socket = Socket::with_pid(Pid::new(), "room:lobby");
        assert_eq!(socket.assigns, ());

        let socket = socket.assign(42i32);
        assert_eq!(socket.assigns, 42);
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
