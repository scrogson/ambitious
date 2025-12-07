//! Presence types.

use crate::core::{DecodeError, Pid};
use crate::message::{Message, decode_payload, encode_payload, encode_with_tag};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// A unique reference for a presence entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PresenceRef(String);

impl Message for PresenceRef {
    fn tag() -> &'static str {
        "PresenceRef"
    }

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &encode_payload(self))
    }

    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode_payload(bytes)
    }

    fn encode_remote(&self) -> Vec<u8> {
        self.encode_local()
    }

    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_local(bytes)
    }
}

impl PresenceRef {
    /// Generate a new unique reference.
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let node_atom = crate::core::node::node_name_atom();
        let node = node_atom.as_str().to_string();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);

        Self(format!("{}:{}:{}", node, timestamp, count))
    }

    /// Create a PresenceRef from a string.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the ref as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Get the node that created this ref.
    pub fn node(&self) -> &str {
        self.0.split(':').next().unwrap_or("unknown")
    }
}

impl Default for PresenceRef {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata for a single presence entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceMeta {
    /// Unique reference for this presence entry.
    pub phx_ref: PresenceRef,
    /// Previous reference (for updates).
    pub phx_ref_prev: Option<PresenceRef>,
    /// The process ID that owns this presence.
    pub pid: Pid,
    /// Custom metadata (serialized).
    pub meta: Vec<u8>,
    /// When this presence was last updated.
    pub updated_at: u64,
}

impl PresenceMeta {
    /// Create new presence metadata.
    pub fn new<M: Serialize>(pid: Pid, meta: &M) -> Self {
        Self {
            phx_ref: PresenceRef::new(),
            phx_ref_prev: None,
            pid,
            meta: postcard::to_allocvec(meta).unwrap_or_default(),
            updated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    /// Decode the custom metadata.
    pub fn decode<M: DeserializeOwned>(&self) -> Option<M> {
        postcard::from_bytes(&self.meta).ok()
    }
}

/// Presence state for a single key (e.g., a user).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresenceState {
    /// All metadata entries for this key.
    /// A user can have multiple presences (e.g., multiple tabs/devices).
    pub metas: Vec<PresenceMeta>,
}

/// A diff representing presence changes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresenceDiff {
    /// Keys that joined (new or updated).
    pub joins: HashMap<String, PresenceState>,
    /// Keys that left.
    pub leaves: HashMap<String, PresenceState>,
}

impl PresenceDiff {
    /// Check if the diff is empty.
    pub fn is_empty(&self) -> bool {
        self.joins.is_empty() && self.leaves.is_empty()
    }
}

/// Messages for presence synchronization between nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceMessage {
    /// Delta update - incremental changes.
    Delta {
        /// The topic for this delta.
        topic: String,
        /// The presence diff.
        diff: PresenceDiff,
    },
    /// Full state sync - sent when a new node joins.
    StateSync {
        /// The topic being synced.
        topic: String,
        /// The current presence state.
        state: HashMap<String, PresenceState>,
    },
    /// Request full state sync from all peers (sent on startup).
    SyncRequest {
        /// The PID of the requesting Presence server.
        from: Pid,
    },
    /// Response to sync request - full state for all topics.
    SyncResponse {
        /// All topics and their presence state.
        state: HashMap<String, HashMap<String, PresenceState>>,
    },
}
