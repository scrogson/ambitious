//! Phoenix-style Channels for real-time communication.
//!
//! Channels provide a high-level abstraction for building real-time features
//! with topic-based routing, join authorization, and message handling.
//!
//! # Architecture
//!
//! - **Socket**: Represents a client connection, holds assigns (custom state)
//! - **Channel**: A topic handler that processes joins, messages, and broadcasts
//! - **Topic**: String identifier for routing (supports patterns like `"room:*"`)
//!
//! # Transports
//!
//! Channels can be served over different transports:
//!
//! - **WebSocket** (requires `websocket` feature): Phoenix Channels V2 JSON protocol
//!   for direct compatibility with Phoenix JavaScript clients
//!
//! # Example
//!
//! ```ignore
//! use ambitious::channel::{Channel, Socket, JoinResult, HandleResult};
//! use async_trait::async_trait;
//! use serde::{Deserialize, Serialize};
//!
//! struct RoomChannel;
//!
//! #[async_trait]
//! impl Channel for RoomChannel {
//!     type Assigns = RoomAssigns;
//!     type JoinPayload = JoinRoom;
//!     type InEvent = RoomEvent;
//!     type OutEvent = RoomBroadcast;
//!
//!     fn topic_pattern() -> &'static str {
//!         "room:*"
//!     }
//!
//!     async fn join(
//!         topic: &str,
//!         payload: Self::JoinPayload,
//!         socket: Socket<Self::Assigns>,
//!     ) -> JoinResult<Self::Assigns> {
//!         // Authorize and set up assigns
//!         let room_id = topic.strip_prefix("room:").unwrap();
//!         let assigns = RoomAssigns { room_id: room_id.to_string() };
//!         JoinResult::Ok(socket.assign(assigns))
//!     }
//!
//!     async fn handle_in(
//!         event: &str,
//!         payload: Self::InEvent,
//!         socket: &mut Socket<Self::Assigns>,
//!     ) -> HandleResult<Self::OutEvent> {
//!         match event {
//!             "new_msg" => {
//!                 // Broadcast to all subscribers
//!                 HandleResult::Broadcast {
//!                     event: "new_msg".to_string(),
//!                     payload: RoomBroadcast::Message { text: payload.text },
//!                 }
//!             }
//!             _ => HandleResult::NoReply,
//!         }
//!     }
//! }
//! ```

mod core;

#[cfg(feature = "websocket")]
pub mod websocket;

// Re-export everything from core
pub use core::*;
