//! Distributed Presence tracking for real-time applications.
//!
//! Presence provides a way to track processes across a distributed cluster
//! with custom metadata, automatically handling joins, leaves, and updates.
//!
//! # Overview
//!
//! Similar to Phoenix.Presence, this module uses a CRDT-based approach to
//! track presence information across nodes without a single point of failure.
//! Each node maintains its own view of presence and synchronizes with peers.
//!
//! # Key Concepts
//!
//! - **Topic**: A string identifier grouping related presences (e.g., "room:lobby")
//! - **Key**: Unique identifier within a topic (e.g., user ID)
//! - **Meta**: Custom metadata associated with a presence (e.g., status, typing)
//! - **Ref**: Unique reference for each presence entry (for updates)
//!
//! # Example
//!
//! ```ignore
//! use starlang::presence::Presence;
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
//! Presence::track("room:lobby", "user:123", meta);
//!
//! // Update presence metadata
//! Presence::update("room:lobby", "user:123", UserMeta {
//!     status: "online".into(),
//!     typing: true,
//! });
//!
//! // List all presences in a topic
//! let presences = Presence::list("room:lobby");
//! for (key, metas) in presences {
//!     println!("{}: {:?}", key, metas);
//! }
//!
//! // Untrack when done
//! Presence::untrack("room:lobby", "user:123");
//! ```
//!
//! # Distributed Behavior
//!
//! Presence automatically syncs across nodes using pg (process groups).
//! When presence changes on one node, a delta is broadcast to all other
//! nodes, which merge it into their local state.

use crate::core::{Atom, Pid};
use crate::dist::pg;
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Global presence tracker instance.
static PRESENCE: OnceLock<PresenceTracker> = OnceLock::new();

/// Get or initialize the global presence tracker.
fn presence() -> &'static PresenceTracker {
    PRESENCE.get_or_init(PresenceTracker::new)
}

/// A unique reference for a presence entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PresenceRef(String);

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

/// Messages for presence synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceMessage {
    /// Sync request - asking for current state.
    SyncRequest {
        /// The topic to sync.
        topic: String,
    },
    /// Sync response - sending current state.
    SyncResponse {
        /// The topic being synced.
        topic: String,
        /// The current presence state.
        state: HashMap<String, PresenceState>,
    },
    /// Delta update - incremental changes.
    Delta {
        /// The topic for this delta.
        topic: String,
        /// The presence diff.
        diff: PresenceDiff,
    },
    /// Heartbeat with clock for consistency.
    Heartbeat {
        /// The node sending the heartbeat.
        node: String,
        /// The node's logical clock value.
        clock: u64,
    },
}

/// The main presence tracker.
pub struct PresenceTracker {
    /// Presence state per topic: topic -> (key -> state).
    state: DashMap<String, HashMap<String, PresenceState>>,
    /// Reverse lookup: PID -> set of (topic, key) pairs.
    pid_to_presence: DashMap<Pid, HashSet<(String, String)>>,
    /// Local logical clock for ordering.
    clock: RwLock<u64>,
    /// Last heartbeat time per node.
    node_heartbeats: DashMap<String, Instant>,
}

impl PresenceTracker {
    /// Create a new presence tracker.
    pub fn new() -> Self {
        Self {
            state: DashMap::new(),
            pid_to_presence: DashMap::new(),
            clock: RwLock::new(0),
            node_heartbeats: DashMap::new(),
        }
    }

    /// Track a presence.
    pub fn track<M: Serialize>(&self, topic: &str, key: &str, pid: Pid, meta: &M) -> PresenceRef {
        let presence_meta = PresenceMeta::new(pid, meta);
        let phx_ref = presence_meta.phx_ref.clone();

        // Join the presence pg group for this topic so we receive deltas from other nodes
        let presence_group = format!("presence:{}", topic);
        pg::join(&presence_group, pid);

        // Add to state
        let mut topic_state = self.state.entry(topic.to_string()).or_default();
        let key_state = topic_state.entry(key.to_string()).or_default();
        key_state.metas.push(presence_meta.clone());

        // Track reverse lookup
        self.pid_to_presence
            .entry(pid)
            .or_default()
            .insert((topic.to_string(), key.to_string()));

        // Broadcast delta
        let diff = PresenceDiff {
            joins: [(
                key.to_string(),
                PresenceState {
                    metas: vec![presence_meta],
                },
            )]
            .into_iter()
            .collect(),
            leaves: HashMap::new(),
        };
        self.broadcast_delta(topic, diff);

        phx_ref
    }

    /// Untrack a presence by key.
    pub fn untrack(&self, topic: &str, key: &str, pid: Pid) {
        let mut removed_metas = Vec::new();

        // Remove from state
        if let Some(mut topic_state) = self.state.get_mut(topic)
            && let Some(key_state) = topic_state.get_mut(key) {
                // Remove entries for this PID
                removed_metas = key_state.metas.drain(..).filter(|m| m.pid == pid).collect();
                key_state.metas.retain(|m| m.pid != pid);

                // Clean up empty key
                if key_state.metas.is_empty() {
                    topic_state.remove(key);
                }
            }

        // Update reverse lookup
        if let Some(mut presences) = self.pid_to_presence.get_mut(&pid) {
            presences.remove(&(topic.to_string(), key.to_string()));
        }

        // Broadcast delta if we removed something
        if !removed_metas.is_empty() {
            let diff = PresenceDiff {
                joins: HashMap::new(),
                leaves: [(
                    key.to_string(),
                    PresenceState {
                        metas: removed_metas,
                    },
                )]
                .into_iter()
                .collect(),
            };
            self.broadcast_delta(topic, diff);
        }
    }

    /// Untrack all presences for a PID.
    pub fn untrack_all(&self, pid: Pid) {
        if let Some((_, presences)) = self.pid_to_presence.remove(&pid) {
            // Group by topic for efficient delta broadcasting
            let mut by_topic: HashMap<String, Vec<(String, Vec<PresenceMeta>)>> = HashMap::new();

            for (topic, key) in presences {
                let mut removed_metas = Vec::new();

                if let Some(mut topic_state) = self.state.get_mut(&topic)
                    && let Some(key_state) = topic_state.get_mut(&key) {
                        removed_metas = key_state
                            .metas
                            .iter()
                            .filter(|m| m.pid == pid)
                            .cloned()
                            .collect();
                        key_state.metas.retain(|m| m.pid != pid);

                        if key_state.metas.is_empty() {
                            topic_state.remove(&key);
                        }
                    }

                if !removed_metas.is_empty() {
                    by_topic
                        .entry(topic)
                        .or_default()
                        .push((key, removed_metas));
                }
            }

            // Broadcast deltas per topic
            for (topic, leaves) in by_topic {
                let diff = PresenceDiff {
                    joins: HashMap::new(),
                    leaves: leaves
                        .into_iter()
                        .map(|(k, metas)| (k, PresenceState { metas }))
                        .collect(),
                };
                self.broadcast_delta(&topic, diff);
            }
        }
    }

    /// Update presence metadata.
    pub fn update<M: Serialize>(
        &self,
        topic: &str,
        key: &str,
        pid: Pid,
        meta: &M,
    ) -> Option<PresenceRef> {
        let mut new_meta = PresenceMeta::new(pid, meta);
        let new_ref = new_meta.phx_ref.clone();
        let mut old_meta = None;

        if let Some(mut topic_state) = self.state.get_mut(topic)
            && let Some(key_state) = topic_state.get_mut(key) {
                // Find and update the entry for this PID
                for m in &mut key_state.metas {
                    if m.pid == pid {
                        new_meta.phx_ref_prev = Some(m.phx_ref.clone());
                        old_meta = Some(m.clone());
                        *m = new_meta.clone();
                        break;
                    }
                }
            }

        // Broadcast delta
        if old_meta.is_some() {
            let diff = PresenceDiff {
                joins: [(
                    key.to_string(),
                    PresenceState {
                        metas: vec![new_meta],
                    },
                )]
                .into_iter()
                .collect(),
                leaves: HashMap::new(),
            };
            self.broadcast_delta(topic, diff);
            Some(new_ref)
        } else {
            None
        }
    }

    /// List all presences for a topic.
    pub fn list(&self, topic: &str) -> HashMap<String, PresenceState> {
        self.state.get(topic).map(|s| s.clone()).unwrap_or_default()
    }

    /// Get presence for a specific key in a topic.
    pub fn get(&self, topic: &str, key: &str) -> Option<PresenceState> {
        self.state.get(topic).and_then(|s| s.get(key).cloned())
    }

    /// Get all keys present in a topic.
    pub fn keys(&self, topic: &str) -> Vec<String> {
        self.state
            .get(topic)
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Count presences in a topic.
    pub fn count(&self, topic: &str) -> usize {
        self.state
            .get(topic)
            .map(|s| s.values().map(|v| v.metas.len()).sum())
            .unwrap_or(0)
    }

    /// Broadcast a delta to other nodes.
    fn broadcast_delta(&self, topic: &str, diff: PresenceDiff) {
        if diff.is_empty() {
            return;
        }

        let msg = PresenceMessage::Delta {
            topic: topic.to_string(),
            diff,
        };

        if let Ok(payload) = postcard::to_allocvec(&msg) {
            // Use pg to broadcast to presence group
            let group = format!("presence:{}", topic);
            let members = pg::get_members(&group);
            let my_node = crate::core::node::node_name_atom();

            for pid in members {
                // Don't send to ourselves (different node)
                if pid.node() != my_node {
                    let _ = crate::send_raw(pid, payload.clone());
                }
            }
        }
    }

    /// Handle an incoming presence message.
    pub fn handle_message(&self, msg: PresenceMessage, _from_node: Atom) {
        match msg {
            PresenceMessage::SyncRequest { topic } => {
                // Send our current state
                let state = self.list(&topic);
                let response = PresenceMessage::SyncResponse { topic, state };
                // TODO: Send response back to requesting node
                tracing::debug!(?response, "Would send sync response");
            }
            PresenceMessage::SyncResponse { topic, state } => {
                // Merge remote state into ours
                self.merge_state(&topic, state);
            }
            PresenceMessage::Delta { topic, diff } => {
                // Apply the delta
                self.apply_delta(&topic, diff);
            }
            PresenceMessage::Heartbeat { node, clock } => {
                // Update heartbeat tracking
                self.node_heartbeats.insert(node, Instant::now());
                // Update our clock if remote is ahead
                let mut our_clock = self.clock.write();
                if clock > *our_clock {
                    *our_clock = clock;
                }
            }
        }
    }

    /// Merge remote state into local state.
    fn merge_state(&self, topic: &str, remote: HashMap<String, PresenceState>) {
        let mut topic_state = self.state.entry(topic.to_string()).or_default();

        for (key, remote_state) in remote {
            let local_state = topic_state.entry(key).or_default();

            // Merge metas - add any we don't have
            for remote_meta in remote_state.metas {
                let exists = local_state
                    .metas
                    .iter()
                    .any(|m| m.phx_ref == remote_meta.phx_ref);
                if !exists {
                    local_state.metas.push(remote_meta);
                }
            }
        }
    }

    /// Apply a delta to local state.
    fn apply_delta(&self, topic: &str, diff: PresenceDiff) {
        let has_joins = !diff.joins.is_empty();

        let mut topic_state = self.state.entry(topic.to_string()).or_default();

        // Process leaves first
        for (key, leave_state) in diff.leaves {
            if let Some(local_state) = topic_state.get_mut(&key) {
                for leave_meta in &leave_state.metas {
                    local_state
                        .metas
                        .retain(|m| m.phx_ref != leave_meta.phx_ref);
                }
                if local_state.metas.is_empty() {
                    topic_state.remove(&key);
                }
            }
        }

        // Process joins
        for (key, join_state) in diff.joins {
            let local_state = topic_state.entry(key).or_default();
            for join_meta in join_state.metas {
                // Remove old version if this is an update
                if let Some(ref prev) = join_meta.phx_ref_prev {
                    local_state.metas.retain(|m| &m.phx_ref != prev);
                }
                // Add new meta if not already present
                let exists = local_state
                    .metas
                    .iter()
                    .any(|m| m.phx_ref == join_meta.phx_ref);
                if !exists {
                    local_state.metas.push(join_meta);
                }
            }
        }

        // Drop the mutable borrow before calling broadcast
        drop(topic_state);

        // If we received a join, send our current state back so new joiners get the full picture
        if has_joins {
            let our_state = self.list(topic);
            if !our_state.is_empty() {
                self.broadcast_state(topic, our_state);
            }
        }
    }

    /// Broadcast our full state for a topic to other nodes.
    fn broadcast_state(&self, topic: &str, state: HashMap<String, PresenceState>) {
        let msg = PresenceMessage::SyncResponse {
            topic: topic.to_string(),
            state,
        };

        if let Ok(payload) = postcard::to_allocvec(&msg) {
            let group = format!("presence:{}", topic);
            let members = pg::get_members(&group);
            let my_node = crate::core::node::node_name_atom();

            for pid in members {
                // Don't send to ourselves (different node)
                if pid.node() != my_node {
                    let _ = crate::send_raw(pid, payload.clone());
                }
            }
        }
    }

    /// Join the presence pg group for a topic.
    pub fn join_group(&self, topic: &str, pid: Pid) {
        let group = format!("presence:{}", topic);
        pg::join(&group, pid);
    }

    /// Leave the presence pg group for a topic.
    pub fn leave_group(&self, topic: &str, pid: Pid) {
        let group = format!("presence:{}", topic);
        pg::leave(&group, pid);
    }
}

impl Default for PresenceTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Public API - Convenience functions using global tracker
// ============================================================================

/// Track a presence in a topic.
///
/// # Arguments
///
/// * `topic` - The topic to track in (e.g., "room:lobby")
/// * `key` - Unique key within the topic (e.g., "user:123")
/// * `meta` - Custom metadata to associate with this presence
///
/// # Returns
///
/// A unique reference for this presence entry.
pub fn track<M: Serialize>(topic: &str, key: &str, meta: M) -> PresenceRef {
    presence().track(topic, key, crate::current_pid(), &meta)
}

/// Track a presence for a specific PID.
pub fn track_pid<M: Serialize>(topic: &str, key: &str, pid: Pid, meta: M) -> PresenceRef {
    presence().track(topic, key, pid, &meta)
}

/// Untrack a presence.
pub fn untrack(topic: &str, key: &str) {
    presence().untrack(topic, key, crate::current_pid());
}

/// Untrack a presence for a specific PID.
pub fn untrack_pid(topic: &str, key: &str, pid: Pid) {
    presence().untrack(topic, key, pid);
}

/// Untrack all presences for a PID.
pub fn untrack_all(pid: Pid) {
    presence().untrack_all(pid);
}

/// Update presence metadata.
pub fn update<M: Serialize>(topic: &str, key: &str, meta: M) -> Option<PresenceRef> {
    presence().update(topic, key, crate::current_pid(), &meta)
}

/// List all presences in a topic.
pub fn list(topic: &str) -> HashMap<String, PresenceState> {
    presence().list(topic)
}

/// Get presence for a specific key.
pub fn get(topic: &str, key: &str) -> Option<PresenceState> {
    presence().get(topic, key)
}

/// Get all keys in a topic.
pub fn keys(topic: &str) -> Vec<String> {
    presence().keys(topic)
}

/// Count presences in a topic.
pub fn count(topic: &str) -> usize {
    presence().count(topic)
}

/// Handle an incoming presence message (for internal use).
pub fn handle_message(msg: PresenceMessage, from_node: Atom) {
    presence().handle_message(msg, from_node);
}

/// Join the presence sync group for a topic.
pub fn join_group(topic: &str) {
    presence().join_group(topic, crate::current_pid());
}

/// Leave the presence sync group for a topic.
pub fn leave_group(topic: &str) {
    presence().leave_group(topic, crate::current_pid());
}

/// Get the global presence tracker (for advanced use).
pub fn tracker() -> &'static PresenceTracker {
    presence()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
    struct TestMeta {
        status: String,
    }

    #[test]
    fn test_track_and_list() {
        let tracker = PresenceTracker::new();
        let pid = Pid::new();
        let meta = TestMeta {
            status: "online".to_string(),
        };

        tracker.track("room:test", "user:1", pid, &meta);

        let presences = tracker.list("room:test");
        assert_eq!(presences.len(), 1);
        assert!(presences.contains_key("user:1"));

        let state = &presences["user:1"];
        assert_eq!(state.metas.len(), 1);

        let decoded: TestMeta = state.metas[0].decode().unwrap();
        assert_eq!(decoded.status, "online");
    }

    #[test]
    fn test_untrack() {
        let tracker = PresenceTracker::new();
        let pid = Pid::new();
        let meta = TestMeta {
            status: "online".to_string(),
        };

        tracker.track("room:test", "user:1", pid, &meta);
        assert_eq!(tracker.count("room:test"), 1);

        tracker.untrack("room:test", "user:1", pid);
        assert_eq!(tracker.count("room:test"), 0);
    }

    #[test]
    fn test_update() {
        let tracker = PresenceTracker::new();
        let pid = Pid::new();

        tracker.track(
            "room:test",
            "user:1",
            pid,
            &TestMeta {
                status: "online".to_string(),
            },
        );

        tracker.update(
            "room:test",
            "user:1",
            pid,
            &TestMeta {
                status: "away".to_string(),
            },
        );

        let state = tracker.get("room:test", "user:1").unwrap();
        let decoded: TestMeta = state.metas[0].decode().unwrap();
        assert_eq!(decoded.status, "away");
        assert!(state.metas[0].phx_ref_prev.is_some());
    }

    #[test]
    fn test_multiple_presences() {
        let tracker = PresenceTracker::new();
        let pid1 = Pid::new();
        let pid2 = Pid::new();
        let meta = TestMeta {
            status: "online".to_string(),
        };

        // Same user, different devices/tabs
        tracker.track("room:test", "user:1", pid1, &meta);
        tracker.track("room:test", "user:1", pid2, &meta);

        let state = tracker.get("room:test", "user:1").unwrap();
        assert_eq!(state.metas.len(), 2);
    }

    #[test]
    fn test_untrack_all() {
        let tracker = PresenceTracker::new();
        let pid = Pid::new();
        let meta = TestMeta {
            status: "online".to_string(),
        };

        tracker.track("room:1", "user:1", pid, &meta);
        tracker.track("room:2", "user:1", pid, &meta);

        tracker.untrack_all(pid);

        assert_eq!(tracker.count("room:1"), 0);
        assert_eq!(tracker.count("room:2"), 0);
    }

    #[test]
    fn test_presence_diff() {
        let tracker = PresenceTracker::new();
        let pid = Pid::new();

        // Simulate receiving a delta from another node
        let diff = PresenceDiff {
            joins: [(
                "user:remote".to_string(),
                PresenceState {
                    metas: vec![PresenceMeta::new(
                        pid,
                        &TestMeta {
                            status: "online".to_string(),
                        },
                    )],
                },
            )]
            .into_iter()
            .collect(),
            leaves: HashMap::new(),
        };

        tracker.apply_delta("room:test", diff);

        assert_eq!(tracker.count("room:test"), 1);
        assert!(tracker.get("room:test", "user:remote").is_some());
    }
}
