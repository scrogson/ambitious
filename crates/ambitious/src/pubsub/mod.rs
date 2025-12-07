//! Phoenix-style PubSub for distributed publish-subscribe messaging.
//!
//! PubSub provides a way to broadcast messages to subscribers across a distributed
//! cluster. Each node runs a PubSub process that coordinates with other nodes via
//! process groups (pg).
//!
//! # Architecture
//!
//! Unlike a static global registry, PubSub is a **named process** that must be started
//! in your application's supervision tree. This follows the Phoenix.PubSub design:
//!
//! - Each PubSub instance is identified by a name (atom)
//! - The PubSub process joins a pg group for cross-node coordination
//! - Local subscriptions use an efficient Registry for fast dispatch
//! - Remote broadcasts are forwarded via pg group members
//!
//! # Example
//!
//! ```ignore
//! use ambitious::pubsub::{PubSub, PubSubConfig};
//!
//! // Start PubSub as part of your application
//! let config = PubSubConfig::new("my_pubsub");
//! PubSub::start_link(config).await?;
//!
//! // Subscribe the current process to a topic
//! PubSub::subscribe("my_pubsub", "room:lobby").await;
//!
//! // Broadcast to all subscribers (local + remote)
//! PubSub::broadcast("my_pubsub", "room:lobby", &MyMessage { ... }).await;
//!
//! // Broadcast excluding the sender
//! PubSub::broadcast_from("my_pubsub", "room:lobby", sender_pid, &MyMessage { ... }).await;
//!
//! // Local-only broadcast (same node)
//! PubSub::local_broadcast("my_pubsub", "room:lobby", &MyMessage { ... }).await;
//! ```
//!
//! # Distributed Behavior
//!
//! When you broadcast a message:
//! 1. Local subscribers receive the message directly via Registry (efficient, no serialization)
//! 2. The message is forwarded to PubSub processes on other nodes via pg
//! 3. Remote PubSub processes dispatch to their local subscribers
//!
//! This design minimizes network traffic - each message crosses the network once per node,
//! not once per subscriber.

mod server_v3;

// v3 is now the primary implementation
pub use server_v3::{
    PubSub, PubSubCall, PubSubCast, PubSubConfig, PubSubInfo, PubSubReply,
};
