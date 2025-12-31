//! Phoenix-style Channels for real-time communication.
//!
//! Channels provide a high-level abstraction for building real-time features
//! with topic-based routing, join authorization, and message handling.
//!
//! # Architecture
//!
//! - **Channel**: The struct IS the channel state
//! - **Socket**: Connection metadata (PID, topic, join_ref)
//! - **Transport**: Handles wire protocol and connection management
//! - **Serializer**: Encodes/decodes messages at the transport boundary
//! - **Topic**: String identifier for routing (supports patterns like `"room:*"`)
//!
//! # Transports
//!
//! Channels can be served over different transports. The `Transport` trait
//! allows you to define custom transports for any connection type.
//!
//! Built-in transports:
//! - **WebSocket** (requires `websocket` feature): Phoenix Channels V2 JSON protocol
//!   for direct compatibility with Phoenix JavaScript clients
//!
//! Custom transports can be implemented using the `Transport` trait and
//! `run_transport` helper function.
//!
//! # Example
//!
//! ```ignore
//! use ambitious::channel::{Channel, Socket, JoinResult, HandleResult, ReplyStatus, async_trait};
//! use ambitious::{Message, handle_in};
//! use serde::{Deserialize, Serialize};
//!
//! // The struct IS the channel state
//! pub struct LobbyChannel {
//!     nick: Option<String>,
//! }
//!
//! #[derive(Serialize, Deserialize)]
//! pub struct JoinPayload {
//!     nick: Option<String>,
//! }
//!
//! #[derive(Message)]
//! pub struct ListRooms;
//!
//! #[derive(Message)]
//! pub struct RoomList {
//!     rooms: Vec<String>,
//! }
//!
//! #[async_trait]
//! impl Channel for LobbyChannel {
//!     type JoinPayload = JoinPayload;
//!     const TOPIC_PATTERN: &'static str = "lobby:*";
//!
//!     async fn join(_topic: &str, payload: JoinPayload, _socket: &Socket) -> JoinResult<Self> {
//!         JoinResult::Ok(Self { nick: payload.nick })
//!     }
//! }
//!
//! #[handle_in("list_rooms")]
//! impl HandleIn<ListRooms> for LobbyChannel {
//!     type Reply = RoomList;
//!
//!     async fn handle_in(&mut self, _msg: ListRooms, _socket: &Socket) -> HandleResult<RoomList> {
//!         HandleResult::Reply {
//!             status: ReplyStatus::Ok,
//!             payload: RoomList { rooms: vec!["lobby".into()] },
//!         }
//!     }
//! }
//! ```

mod core;
pub mod transport;

#[cfg(feature = "websocket")]
pub mod websocket;

// Re-export everything from core
pub use core::*;

// Re-export transport types
pub use transport::{
    ConnectionMetadata, ControlMessage, Transport, TransportBuilder, TransportConfig,
    TransportMessage, TransportReply, TransportResult, run_transport_loop,
};
