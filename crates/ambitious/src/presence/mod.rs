//! Distributed Presence tracking for real-time applications.
//!
//! Presence provides real-time tracking of processes across a distributed cluster
//! with custom metadata, automatically handling joins, leaves, and updates.
//!
//! # Architecture
//!
//! Like Phoenix.Presence, this module is built on top of PubSub:
//!
//! - **Presence** is a GenServer that tracks presence state and broadcasts diffs
//! - **PubSub** handles the actual message distribution to subscribers
//! - Each node runs its own Presence process that syncs with other nodes
//!
//! # Starting Presence
//!
//! Presence must be started as part of your supervision tree, after PubSub:
//!
//! ```ignore
//! // First start PubSub
//! PubSub::start_link(PubSubConfig::new("my_pubsub")).await?;
//!
//! // Then start Presence
//! Presence::start_link(PresenceConfig::new("my_presence", "my_pubsub")).await?;
//! ```
//!
//! # Example
//!
//! ```ignore
//! use ambitious::presence::Presence;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Clone, Serialize, Deserialize)]
//! struct UserMeta {
//!     status: String,
//!     typing: bool,
//! }
//!
//! // Track a user's presence
//! let meta = UserMeta { status: "online".into(), typing: false };
//! Presence::track("my_presence", "room:lobby", "user:123", &meta).await?;
//!
//! // Update presence metadata
//! Presence::update("my_presence", "room:lobby", "user:123", &UserMeta {
//!     status: "online".into(),
//!     typing: true,
//! }).await?;
//!
//! // List all presences in a topic
//! let presences = Presence::list("my_presence", "room:lobby").await?;
//! for (key, metas) in presences {
//!     println!("{}: {:?}", key, metas);
//! }
//!
//! // Untrack when done
//! Presence::untrack("my_presence", "room:lobby", "user:123").await?;
//! ```
//!
//! # Distributed Behavior
//!
//! When presence changes on one node:
//! 1. The local Presence server updates its state
//! 2. A diff is broadcast via PubSub to all nodes
//! 3. Other Presence servers receive the diff and merge it
//!
//! This provides eventually consistent presence across the cluster.

mod crdt;
mod server;
mod types;

pub use crdt::{
    Clock, MergeResult, PresenceCrdt, PresenceDelta, Replica, ReplicaStatus, Tag, TrackedEntry,
};
pub use server::{Presence, PresenceConfig};
pub use types::{PresenceDiff, PresenceMessage, PresenceMeta, PresenceRef, PresenceState};
