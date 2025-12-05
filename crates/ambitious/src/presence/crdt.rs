//! ORSWOT CRDT (Observed-Remove Set Without Tombstones) for Presence tracking.
//!
//! This implements a delta-state CRDT similar to Phoenix.Tracker.State, providing:
//! - Causal context tracking per replica (node)
//! - Unique tags for conflict-free merge
//! - Delta-based synchronization
//! - Replica up/down filtering

use crate::core::Pid;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A replica identifier (node name).
pub type Replica = String;

/// Logical clock for causal ordering.
pub type Clock = u64;

/// Unique tag for each presence entry.
/// Tags consist of (replica, clock) and uniquely identify each tracked entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tag {
    /// The replica (node) that created this entry.
    pub replica: Replica,
    /// The logical clock value when created.
    pub clock: Clock,
}

impl Tag {
    /// Create a new tag.
    pub fn new(replica: impl Into<Replica>, clock: Clock) -> Self {
        Self {
            replica: replica.into(),
            clock,
        }
    }
}

/// A tracked presence entry with CRDT semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedEntry {
    /// The topic this entry belongs to.
    pub topic: String,
    /// The process ID that owns this presence.
    pub pid: Pid,
    /// The key identifying this presence within the topic.
    pub key: String,
    /// Custom metadata (serialized).
    pub meta: Vec<u8>,
    /// Unique tag for conflict resolution.
    pub tag: Tag,
}

impl TrackedEntry {
    /// Create a new tracked entry.
    pub fn new(
        topic: impl Into<String>,
        pid: Pid,
        key: impl Into<String>,
        meta: Vec<u8>,
        tag: Tag,
    ) -> Self {
        Self {
            topic: topic.into(),
            pid,
            key: key.into(),
            meta,
            tag,
        }
    }
}

/// Replica status for filtering.
#[derive(Debug, Clone, Default)]
pub enum ReplicaStatus {
    /// Replica is up and its entries should be visible.
    #[default]
    Up,
    /// Replica is down (temporarily or permanently).
    Down {
        /// Timestamp when marked down (epoch seconds).
        since: u64,
        /// Whether this is a temporary disconnect.
        temporary: bool,
    },
}

/// Delta state for synchronization.
/// Contains incremental changes since last sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresenceDelta {
    /// Entries that joined.
    pub joins: Vec<TrackedEntry>,
    /// Tags of entries that left.
    pub leave_tags: Vec<Tag>,
    /// Causal context at time of delta.
    pub context: HashMap<Replica, Clock>,
}

impl PresenceDelta {
    /// Check if the delta is empty.
    pub fn is_empty(&self) -> bool {
        self.joins.is_empty() && self.leave_tags.is_empty()
    }

    /// Get all entries from a specific topic.
    pub fn joins_for_topic(&self, topic: &str) -> Vec<&TrackedEntry> {
        self.joins.iter().filter(|e| e.topic == topic).collect()
    }
}

/// Result of merging remote state.
#[derive(Debug, Default)]
pub struct MergeResult {
    /// Entries that were added (joins).
    pub joins: Vec<TrackedEntry>,
    /// Entries that were removed (leaves).
    pub leaves: Vec<TrackedEntry>,
}

impl MergeResult {
    /// Check if the merge had any effect.
    pub fn is_empty(&self) -> bool {
        self.joins.is_empty() && self.leaves.is_empty()
    }
}

/// ORSWOT CRDT state for presence tracking.
///
/// Provides conflict-free replicated presence tracking with:
/// - Unique tagging of entries for deduplication
/// - Causal context for ordering
/// - Delta-based synchronization
/// - Replica up/down filtering
pub struct PresenceCrdt {
    /// Our replica name (node name).
    replica: Replica,
    /// Our logical clock.
    clock: Clock,
    /// Causal context: max clock seen from each replica.
    context: HashMap<Replica, Clock>,
    /// Values indexed by (topic, key) -> entries.
    values: HashMap<(String, String), Vec<TrackedEntry>>,
    /// Reverse lookup: pid -> (topic, key) pairs for fast untrack_all.
    pid_index: HashMap<Pid, Vec<(String, String)>>,
    /// Replica status for filtering.
    replicas: HashMap<Replica, ReplicaStatus>,
    /// Pending delta since last broadcast.
    delta: PresenceDelta,
}

impl PresenceCrdt {
    /// Create new CRDT state for the given replica.
    pub fn new(replica: impl Into<Replica>) -> Self {
        let replica = replica.into();
        Self {
            replica: replica.clone(),
            clock: 0,
            context: HashMap::from([(replica, 0)]),
            values: HashMap::new(),
            pid_index: HashMap::new(),
            replicas: HashMap::new(),
            delta: PresenceDelta::default(),
        }
    }

    /// Get our replica name.
    pub fn replica(&self) -> &str {
        &self.replica
    }

    /// Get current causal context.
    pub fn context(&self) -> &HashMap<Replica, Clock> {
        &self.context
    }

    /// Track a new presence (join operation).
    ///
    /// Returns the tag assigned to this entry.
    pub fn track(
        &mut self,
        topic: impl Into<String>,
        pid: Pid,
        key: impl Into<String>,
        meta: Vec<u8>,
    ) -> Tag {
        let topic = topic.into();
        let key = key.into();

        // 1. Increment our clock
        self.clock += 1;
        let tag = Tag::new(self.replica.clone(), self.clock);

        // 2. Update our context
        self.context.insert(self.replica.clone(), self.clock);

        // 3. Create the entry
        let entry = TrackedEntry::new(topic.clone(), pid, key.clone(), meta, tag.clone());

        // 4. Add to values
        self.values
            .entry((topic.clone(), key.clone()))
            .or_default()
            .push(entry.clone());

        // 5. Update pid index
        self.pid_index.entry(pid).or_default().push((topic, key));

        // 6. Record in delta
        self.delta.joins.push(entry);
        self.delta.context = self.context.clone();

        tag
    }

    /// Untrack a presence for a specific pid (leave operation).
    ///
    /// Returns the tags of removed entries.
    pub fn untrack(&mut self, topic: &str, key: &str, pid: Pid) -> Vec<Tag> {
        let mut removed_tags = Vec::new();

        if let Some(entries) = self.values.get_mut(&(topic.to_string(), key.to_string())) {
            // Find entries to remove
            let to_remove: Vec<_> = entries
                .iter()
                .filter(|e| e.pid == pid)
                .map(|e| e.tag.clone())
                .collect();

            // Remove them
            entries.retain(|e| e.pid != pid);

            // Record leave tags
            for tag in &to_remove {
                self.delta.leave_tags.push(tag.clone());
            }
            removed_tags = to_remove;
        }

        // Clean up empty entries
        self.values.retain(|_, entries| !entries.is_empty());

        // Update pid index
        if let Some(keys) = self.pid_index.get_mut(&pid) {
            keys.retain(|(t, k)| t != topic || k != key);
            if keys.is_empty() {
                self.pid_index.remove(&pid);
            }
        }

        // Update delta context
        if !removed_tags.is_empty() {
            self.delta.context = self.context.clone();
        }

        removed_tags
    }

    /// Untrack all presences for a pid.
    ///
    /// Returns all removed entries grouped by topic.
    pub fn untrack_all(&mut self, pid: Pid) -> HashMap<String, Vec<TrackedEntry>> {
        let mut removed_by_topic: HashMap<String, Vec<TrackedEntry>> = HashMap::new();

        // Get all (topic, key) pairs for this pid
        if let Some(keys) = self.pid_index.remove(&pid) {
            for (topic, key) in keys {
                if let Some(entries) = self.values.get_mut(&(topic.clone(), key.clone())) {
                    // Find and remove entries for this pid
                    let mut i = 0;
                    while i < entries.len() {
                        if entries[i].pid == pid {
                            let entry = entries.remove(i);
                            self.delta.leave_tags.push(entry.tag.clone());
                            removed_by_topic
                                .entry(topic.clone())
                                .or_default()
                                .push(entry);
                        } else {
                            i += 1;
                        }
                    }
                }
            }
        }

        // Clean up empty entries
        self.values.retain(|_, entries| !entries.is_empty());

        // Update delta context
        if !removed_by_topic.is_empty() {
            self.delta.context = self.context.clone();
        }

        removed_by_topic
    }

    /// List all presences for a topic.
    ///
    /// Returns entries grouped by key, filtered by replica status.
    pub fn list(&self, topic: &str) -> HashMap<String, Vec<TrackedEntry>> {
        let mut result: HashMap<String, Vec<TrackedEntry>> = HashMap::new();

        for ((t, k), entries) in &self.values {
            if t != topic {
                continue;
            }

            // Filter to only include entries from up replicas
            let online_entries: Vec<_> = entries
                .iter()
                .filter(|e| self.is_replica_up(&e.tag.replica))
                .cloned()
                .collect();

            if !online_entries.is_empty() {
                result.insert(k.clone(), online_entries);
            }
        }

        result
    }

    /// List all presences across all topics.
    pub fn list_all(&self) -> HashMap<String, HashMap<String, Vec<TrackedEntry>>> {
        let mut result: HashMap<String, HashMap<String, Vec<TrackedEntry>>> = HashMap::new();

        for ((topic, key), entries) in &self.values {
            let online_entries: Vec<_> = entries
                .iter()
                .filter(|e| self.is_replica_up(&e.tag.replica))
                .cloned()
                .collect();

            if !online_entries.is_empty() {
                result
                    .entry(topic.clone())
                    .or_default()
                    .insert(key.clone(), online_entries);
            }
        }

        result
    }

    /// Get a specific presence.
    pub fn get(&self, topic: &str, key: &str) -> Option<Vec<TrackedEntry>> {
        self.values
            .get(&(topic.to_string(), key.to_string()))
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| self.is_replica_up(&e.tag.replica))
                    .cloned()
                    .collect()
            })
            .filter(|entries: &Vec<TrackedEntry>| !entries.is_empty())
    }

    /// Merge remote delta into local state.
    ///
    /// Uses causal context for conflict resolution:
    /// - Only adds entries with tags we haven't seen
    /// - Removes entries whose tags are in leave_tags
    pub fn merge(&mut self, remote: &PresenceDelta) -> MergeResult {
        let mut result = MergeResult::default();

        // Capture our context at the start of merge - this is what we use to
        // determine if entries are "new" to us. We don't want to update context
        // during join processing, as that would incorrectly reject entries that
        // arrive out of order (e.g., clock 2 before clock 1).
        let original_context = self.context.clone();

        // Process leaves first (by tag)
        for leave_tag in &remote.leave_tags {
            // Find and remove entries with this tag
            for entries in self.values.values_mut() {
                let mut i = 0;
                while i < entries.len() {
                    if entries[i].tag == *leave_tag {
                        let entry = entries.remove(i);
                        result.leaves.push(entry);
                    } else {
                        i += 1;
                    }
                }
            }
        }

        // Clean up empty entries after leaves
        self.values.retain(|_, entries| !entries.is_empty());

        // Process joins (only add if we haven't seen this tag based on ORIGINAL context)
        for entry in &remote.joins {
            // Check if we've already seen this clock value from this replica
            // Use the original context, not our updated one
            let dominated = original_context
                .get(&entry.tag.replica)
                .map(|&clock| entry.tag.clock <= clock)
                .unwrap_or(false);

            if !dominated {
                // We haven't seen this entry, add it
                let key = (entry.topic.clone(), entry.key.clone());

                // Check for duplicates by tag
                let exists = self
                    .values
                    .get(&key)
                    .map(|entries| entries.iter().any(|e| e.tag == entry.tag))
                    .unwrap_or(false);

                if !exists {
                    self.values.entry(key).or_default().push(entry.clone());
                    result.joins.push(entry.clone());

                    // Update pid index
                    self.pid_index
                        .entry(entry.pid)
                        .or_default()
                        .push((entry.topic.clone(), entry.key.clone()));
                }
            }
        }

        // Now update context from remote (after all joins are processed)
        for (replica, &clock) in &remote.context {
            let our_clock = self.context.get(replica).copied().unwrap_or(0);
            if clock > our_clock {
                self.context.insert(replica.clone(), clock);
            }
        }

        result
    }

    /// Extract delta for sync to a remote with the given context.
    ///
    /// Returns entries that the remote hasn't seen (based on their context).
    pub fn extract(&self, remote_context: &HashMap<Replica, Clock>) -> PresenceDelta {
        let mut delta = PresenceDelta::default();

        // Find entries not in remote's causal history
        for entries in self.values.values() {
            for entry in entries {
                let remote_clock = remote_context.get(&entry.tag.replica).copied().unwrap_or(0);

                // If our entry's clock is greater than what remote has seen
                if entry.tag.clock > remote_clock {
                    delta.joins.push(entry.clone());
                }
            }
        }

        delta.context = self.context.clone();
        delta
    }

    /// Extract full state as a delta.
    pub fn extract_all(&self) -> PresenceDelta {
        let mut delta = PresenceDelta::default();

        for entries in self.values.values() {
            for entry in entries {
                delta.joins.push(entry.clone());
            }
        }

        delta.context = self.context.clone();
        delta
    }

    /// Mark replica as up (returns entries that become visible).
    pub fn replica_up(&mut self, replica: &str) -> Vec<TrackedEntry> {
        let was_down = matches!(self.replicas.get(replica), Some(ReplicaStatus::Down { .. }));

        self.replicas.insert(replica.to_string(), ReplicaStatus::Up);

        if was_down {
            // Return entries that are now visible
            self.values
                .values()
                .flatten()
                .filter(|e| e.tag.replica == replica)
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Mark replica as down (returns entries that become hidden).
    pub fn replica_down(&mut self, replica: &str, temporary: bool) -> Vec<TrackedEntry> {
        let was_up = !matches!(self.replicas.get(replica), Some(ReplicaStatus::Down { .. }));

        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.replicas.insert(
            replica.to_string(),
            ReplicaStatus::Down { since, temporary },
        );

        if was_up {
            // Return entries that are now hidden
            self.values
                .values()
                .flatten()
                .filter(|e| e.tag.replica == replica)
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Remove all entries from a permanently down replica.
    pub fn remove_down_replica(&mut self, replica: &str) -> Vec<TrackedEntry> {
        let mut removed = Vec::new();

        for entries in self.values.values_mut() {
            let mut i = 0;
            while i < entries.len() {
                if entries[i].tag.replica == replica {
                    removed.push(entries.remove(i));
                } else {
                    i += 1;
                }
            }
        }

        // Clean up empty entries
        self.values.retain(|_, entries| !entries.is_empty());

        // Remove from replicas
        self.replicas.remove(replica);

        removed
    }

    /// Check if a replica is considered up.
    pub fn is_replica_up(&self, replica: &str) -> bool {
        // If we don't know about the replica, assume it's up
        !matches!(self.replicas.get(replica), Some(ReplicaStatus::Down { .. }))
    }

    /// Get pending delta and reset it.
    pub fn take_delta(&mut self) -> Option<PresenceDelta> {
        if self.delta.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.delta))
        }
    }

    /// Check if there are pending changes.
    pub fn has_delta(&self) -> bool {
        !self.delta.is_empty()
    }

    /// Get the number of entries.
    pub fn len(&self) -> usize {
        self.values.values().map(|v| v.len()).sum()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn make_pid(id: u64) -> Pid {
        // Create a test pid using remote constructor with test node
        Pid::remote("test@localhost", id, 0)
    }

    fn make_pid_for_node(node: &str, id: u64) -> Pid {
        Pid::remote(node, id, 0)
    }

    // =========================================================================
    // Strategies for property-based tests
    // =========================================================================

    /// Generate a topic name
    fn arb_topic() -> impl Strategy<Value = String> {
        "room:[a-z]{1,10}".prop_map(|s| s.to_string())
    }

    /// Generate a user key
    fn arb_key() -> impl Strategy<Value = String> {
        "user:[0-9]{1,5}".prop_map(|s| s.to_string())
    }

    /// Generate metadata bytes
    fn arb_meta() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 0..20)
    }

    /// Generate a PID id
    fn arb_pid_id() -> impl Strategy<Value = u64> {
        0u64..1000
    }

    /// Operations that can be performed on a CRDT
    #[derive(Debug, Clone)]
    enum CrdtOp {
        Track {
            topic: String,
            pid_id: u64,
            key: String,
            meta: Vec<u8>,
        },
        Untrack {
            topic: String,
            key: String,
            pid_id: u64,
        },
        UntrackAll {
            pid_id: u64,
        },
    }

    /// Generate a CRDT operation
    fn arb_crdt_op() -> impl Strategy<Value = CrdtOp> {
        prop_oneof![
            // Track is most common
            4 => (arb_topic(), arb_pid_id(), arb_key(), arb_meta())
                .prop_map(|(topic, pid_id, key, meta)| CrdtOp::Track { topic, pid_id, key, meta }),
            // Untrack
            2 => (arb_topic(), arb_key(), arb_pid_id())
                .prop_map(|(topic, key, pid_id)| CrdtOp::Untrack { topic, key, pid_id }),
            // UntrackAll
            1 => arb_pid_id().prop_map(|pid_id| CrdtOp::UntrackAll { pid_id }),
        ]
    }

    /// Generate a sequence of operations
    fn arb_ops(max_len: usize) -> impl Strategy<Value = Vec<CrdtOp>> {
        prop::collection::vec(arb_crdt_op(), 0..max_len)
    }

    /// Apply an operation to a CRDT
    fn apply_op(crdt: &mut PresenceCrdt, op: &CrdtOp, node: &str) {
        match op {
            CrdtOp::Track {
                topic,
                pid_id,
                key,
                meta,
            } => {
                crdt.track(topic, make_pid_for_node(node, *pid_id), key, meta.clone());
            }
            CrdtOp::Untrack { topic, key, pid_id } => {
                crdt.untrack(topic, key, make_pid_for_node(node, *pid_id));
            }
            CrdtOp::UntrackAll { pid_id } => {
                crdt.untrack_all(make_pid_for_node(node, *pid_id));
            }
        }
    }

    /// Get all entries as a set of (topic, key, tag) for comparison
    fn entries_set(crdt: &PresenceCrdt) -> std::collections::HashSet<(String, String, Tag)> {
        let mut set = std::collections::HashSet::new();
        for ((topic, key), entries) in &crdt.values {
            for entry in entries {
                set.insert((topic.clone(), key.clone(), entry.tag.clone()));
            }
        }
        set
    }

    // =========================================================================
    // Property tests
    // =========================================================================

    proptest! {
        /// Property: Merge is commutative - order of merging deltas doesn't matter
        #[test]
        fn prop_merge_is_commutative(
            ops1 in arb_ops(10),
            ops2 in arb_ops(10),
        ) {
            // Create two source CRDTs
            let mut src1 = PresenceCrdt::new("node1");
            let mut src2 = PresenceCrdt::new("node2");

            // Apply operations
            for op in &ops1 {
                apply_op(&mut src1, op, "node1@localhost");
            }
            for op in &ops2 {
                apply_op(&mut src2, op, "node2@localhost");
            }

            // Extract deltas
            let delta1 = src1.extract_all();
            let delta2 = src2.extract_all();

            // Create two target CRDTs and merge in different orders
            let mut target_a = PresenceCrdt::new("node_a");
            let mut target_b = PresenceCrdt::new("node_b");

            // Order A: delta1 then delta2
            target_a.merge(&delta1);
            target_a.merge(&delta2);

            // Order B: delta2 then delta1
            target_b.merge(&delta2);
            target_b.merge(&delta1);

            // Both should have the same entries
            prop_assert_eq!(entries_set(&target_a), entries_set(&target_b));
        }

        /// Property: Merge is idempotent - merging same delta twice has no effect
        #[test]
        fn prop_merge_is_idempotent(ops in arb_ops(10)) {
            let mut src = PresenceCrdt::new("node1");
            for op in &ops {
                apply_op(&mut src, op, "node1@localhost");
            }

            let delta = src.extract_all();

            let mut target = PresenceCrdt::new("node2");

            // First merge
            target.merge(&delta);
            let after_first = entries_set(&target);

            // Second merge (should be no-op)
            target.merge(&delta);
            let after_second = entries_set(&target);

            prop_assert_eq!(after_first, after_second);
        }

        /// Property: Any sequence of operations on multiple nodes converges
        #[test]
        fn prop_operations_converge(
            ops1 in arb_ops(10),
            ops2 in arb_ops(10),
            ops3 in arb_ops(10),
        ) {
            // Three nodes apply different operations
            let mut node1 = PresenceCrdt::new("node1");
            let mut node2 = PresenceCrdt::new("node2");
            let mut node3 = PresenceCrdt::new("node3");

            for op in &ops1 {
                apply_op(&mut node1, op, "node1@localhost");
            }
            for op in &ops2 {
                apply_op(&mut node2, op, "node2@localhost");
            }
            for op in &ops3 {
                apply_op(&mut node3, op, "node3@localhost");
            }

            // Exchange all deltas in round-robin fashion
            let d1 = node1.extract_all();
            let d2 = node2.extract_all();
            let d3 = node3.extract_all();

            // All nodes merge all deltas
            node1.merge(&d2);
            node1.merge(&d3);
            node2.merge(&d1);
            node2.merge(&d3);
            node3.merge(&d1);
            node3.merge(&d2);

            // All nodes should converge to the same state
            let set1 = entries_set(&node1);
            let set2 = entries_set(&node2);
            let set3 = entries_set(&node3);

            prop_assert_eq!(&set1, &set2, "node1 and node2 should converge");
            prop_assert_eq!(&set2, &set3, "node2 and node3 should converge");
        }

        /// Property: Track then untrack results in no entry
        #[test]
        fn prop_track_untrack_removes_entry(
            topic in arb_topic(),
            key in arb_key(),
            pid_id in arb_pid_id(),
            meta in arb_meta(),
        ) {
            let mut crdt = PresenceCrdt::new("node1");
            let pid = make_pid(pid_id);

            // Track
            crdt.track(&topic, pid, &key, meta);
            prop_assert!(crdt.get(&topic, &key).is_some());

            // Untrack
            crdt.untrack(&topic, &key, pid);
            prop_assert!(crdt.get(&topic, &key).is_none());
        }

        /// Property: Double track creates two entries (same key, same pid)
        /// This is valid because each track gets a unique tag
        #[test]
        fn prop_double_track_creates_distinct_entries(
            topic in arb_topic(),
            key in arb_key(),
            pid_id in arb_pid_id(),
            meta1 in arb_meta(),
            meta2 in arb_meta(),
        ) {
            let mut crdt = PresenceCrdt::new("node1");
            let pid = make_pid(pid_id);

            let tag1 = crdt.track(&topic, pid, &key, meta1);
            let tag2 = crdt.track(&topic, pid, &key, meta2);

            // Tags should be different (different clocks)
            prop_assert_ne!(tag1, tag2);

            // Should have 2 entries under the same key
            let entries = crdt.get(&topic, &key).unwrap();
            prop_assert_eq!(entries.len(), 2);
        }

        /// Property: Clock always increases
        #[test]
        fn prop_clock_always_increases(ops in arb_ops(20)) {
            let mut crdt = PresenceCrdt::new("node1");
            let mut last_clock = 0u64;

            for op in &ops {
                if let CrdtOp::Track { topic, pid_id, key, meta } = op {
                    let tag = crdt.track(topic, make_pid(*pid_id), key, meta.clone());
                    prop_assert!(tag.clock > last_clock);
                    last_clock = tag.clock;
                }
            }
        }

        /// Property: Extract respects causal context
        #[test]
        fn prop_extract_respects_context(n_entries in 1usize..10) {
            let mut crdt = PresenceCrdt::new("node1");

            // Track n entries
            for i in 0..n_entries {
                crdt.track("room:test", make_pid(i as u64), format!("user:{}", i), vec![]);
            }

            // Remote has seen some entries
            let seen = n_entries / 2;
            let remote_context = std::collections::HashMap::from([
                ("node1".to_string(), seen as u64)
            ]);

            let delta = crdt.extract(&remote_context);

            // Should only extract entries with clock > seen
            prop_assert_eq!(delta.joins.len(), n_entries - seen);
            for entry in &delta.joins {
                prop_assert!(entry.tag.clock > seen as u64);
            }
        }

        /// Property: Untrack all removes exactly all entries for that PID
        #[test]
        fn prop_untrack_all_removes_correct_entries(
            topics in prop::collection::vec(arb_topic(), 1..5),
            target_pid_id in arb_pid_id(),
            other_pid_id in arb_pid_id(),
        ) {
            prop_assume!(target_pid_id != other_pid_id);

            let mut crdt = PresenceCrdt::new("node1");
            let target_pid = make_pid(target_pid_id);
            let other_pid = make_pid(other_pid_id);

            // Track target pid in multiple topics
            for (i, topic) in topics.iter().enumerate() {
                crdt.track(topic, target_pid, format!("target:{}", i), vec![]);
            }

            // Track other pid in same topics
            for (i, topic) in topics.iter().enumerate() {
                crdt.track(topic, other_pid, format!("other:{}", i), vec![]);
            }

            let total_before = crdt.len();
            prop_assert_eq!(total_before, topics.len() * 2);

            // Untrack all for target
            let removed = crdt.untrack_all(target_pid);
            let removed_count: usize = removed.values().map(|v| v.len()).sum();

            // Should have removed exactly target's entries
            prop_assert_eq!(removed_count, topics.len());
            prop_assert_eq!(crdt.len(), topics.len()); // Only other_pid entries remain
        }

        /// Property: Replica down filters entries, up shows them again
        #[test]
        fn prop_replica_up_down_is_reversible(ops in arb_ops(10)) {
            let mut node1 = PresenceCrdt::new("node1");
            let mut node2 = PresenceCrdt::new("node2");

            // Apply ops to node1
            for op in &ops {
                apply_op(&mut node1, op, "node1@localhost");
            }

            // Sync to node2
            let delta = node1.extract_all();
            node2.merge(&delta);

            // Remember state before down
            let before_down = crdt_entry_count(&node2, "node1");

            // Mark node1 as down
            node2.replica_down("node1", true);

            // Entries from node1 should be filtered
            prop_assert_eq!(visible_entry_count(&node2, "node1"), 0);

            // Mark node1 as up
            node2.replica_up("node1");

            // Entries should be visible again
            prop_assert_eq!(visible_entry_count(&node2, "node1"), before_down);
        }

        /// Property: Leave propagates correctly through merge
        #[test]
        fn prop_leave_propagates_through_merge(
            topic in arb_topic(),
            key in arb_key(),
            pid_id in arb_pid_id(),
        ) {
            let mut node1 = PresenceCrdt::new("node1");
            let mut node2 = PresenceCrdt::new("node2");
            let pid = make_pid_for_node("node1@localhost", pid_id);

            // Track on node1
            node1.track(&topic, pid, &key, vec![]);

            // Sync to node2
            let delta1 = node1.extract_all();
            node2.merge(&delta1);
            prop_assert_eq!(node2.len(), 1);

            // Untrack on node1
            node1.untrack(&topic, &key, pid);

            // Get leave delta and merge
            let delta2 = node1.take_delta().unwrap();
            prop_assert!(!delta2.leave_tags.is_empty());

            node2.merge(&delta2);

            // Entry should be removed on node2
            prop_assert_eq!(node2.len(), 0);
        }
    }

    // Helper functions for property tests

    /// Count total entries from a specific replica (including filtered)
    fn crdt_entry_count(crdt: &PresenceCrdt, replica: &str) -> usize {
        crdt.values
            .values()
            .flatten()
            .filter(|e| e.tag.replica == replica)
            .count()
    }

    /// Count visible entries from a specific replica
    fn visible_entry_count(crdt: &PresenceCrdt, replica: &str) -> usize {
        if !crdt.is_replica_up(replica) {
            return 0;
        }
        crdt_entry_count(crdt, replica)
    }

    // =========================================================================
    // Regression tests for property test failures
    // =========================================================================

    /// Reproduction of proptest failure - 3 nodes with mixed ops should converge
    #[test]
    fn test_convergence_with_untrack_ops() {
        let mut node1 = PresenceCrdt::new("node1");
        let mut node2 = PresenceCrdt::new("node2");
        let mut node3 = PresenceCrdt::new("node3");

        // ops2 - same as failing proptest
        // Untrack on non-existent (should be no-op)
        node2.untrack(
            "room:jgdxb",
            "user:39884",
            make_pid_for_node("node2@localhost", 329),
        );
        // Track two entries
        node2.track(
            "room:vr",
            make_pid_for_node("node2@localhost", 911),
            "user:6215",
            vec![55],
        );
        node2.track(
            "room:xtp",
            make_pid_for_node("node2@localhost", 260),
            "user:13480",
            vec![],
        );
        // UntrackAll on non-existent (should be no-op)
        node2.untrack_all(make_pid_for_node("node2@localhost", 435));

        // ops3 - same as failing proptest
        // Untrack on non-existent (should be no-op)
        node3.untrack(
            "room:wc",
            "user:54910",
            make_pid_for_node("node3@localhost", 132),
        );
        // Track one entry
        node3.track(
            "room:ufygoims",
            make_pid_for_node("node3@localhost", 310),
            "user:3145",
            vec![161, 223, 75, 253, 15],
        );
        // UntrackAll on non-existent (should be no-op)
        node3.untrack_all(make_pid_for_node("node3@localhost", 207));

        // Debug: check state before sync
        println!("Before sync:");
        println!("  node1: {} entries", node1.len());
        println!(
            "  node2: {} entries, context: {:?}",
            node2.len(),
            node2.context()
        );
        println!(
            "  node3: {} entries, context: {:?}",
            node3.len(),
            node3.context()
        );

        // Extract deltas
        let d1 = node1.extract_all();
        let d2 = node2.extract_all();
        let d3 = node3.extract_all();

        println!("\nDeltas:");
        println!("  d1: {} joins", d1.joins.len());
        println!("  d2: {} joins, context: {:?}", d2.joins.len(), d2.context);
        println!("  d3: {} joins, context: {:?}", d3.joins.len(), d3.context);

        // All nodes merge all deltas
        node1.merge(&d2);
        node1.merge(&d3);
        node2.merge(&d1);
        node2.merge(&d3);
        node3.merge(&d1);
        node3.merge(&d2);

        // Debug: check state after sync
        println!("\nAfter sync:");
        println!("  node1: {} entries", node1.len());
        println!("  node2: {} entries", node2.len());
        println!("  node3: {} entries", node3.len());

        let set1 = entries_set(&node1);
        let set2 = entries_set(&node2);
        let set3 = entries_set(&node3);

        println!("\nEntry sets:");
        println!("  node1: {:?}", set1);
        println!("  node2: {:?}", set2);
        println!("  node3: {:?}", set3);

        // All should have 3 entries and be equal
        assert_eq!(node1.len(), 3, "node1 should have 3 entries");
        assert_eq!(node2.len(), 3, "node2 should have 3 entries");
        assert_eq!(node3.len(), 3, "node3 should have 3 entries");
        assert_eq!(set1, set2, "node1 and node2 should converge");
        assert_eq!(set2, set3, "node2 and node3 should converge");
    }

    // =========================================================================
    // Original unit tests
    // =========================================================================

    #[test]
    fn test_track_creates_unique_tag() {
        let mut crdt = PresenceCrdt::new("node1");
        let tag1 = crdt.track("room:lobby", make_pid(1), "user:1", vec![]);
        let tag2 = crdt.track("room:lobby", make_pid(2), "user:2", vec![]);

        assert_eq!(tag1.replica, "node1");
        assert_eq!(tag1.clock, 1);
        assert_eq!(tag2.replica, "node1");
        assert_eq!(tag2.clock, 2);
        assert_ne!(tag1, tag2);
    }

    #[test]
    fn test_track_increments_clock() {
        let mut crdt = PresenceCrdt::new("node1");

        for i in 1..=5 {
            let tag = crdt.track("room:lobby", make_pid(i), format!("user:{}", i), vec![]);
            assert_eq!(tag.clock, i);
        }

        assert_eq!(crdt.context().get("node1"), Some(&5));
    }

    #[test]
    fn test_list_returns_tracked_entries() {
        let mut crdt = PresenceCrdt::new("node1");

        crdt.track("room:lobby", make_pid(1), "user:1", vec![1, 2, 3]);
        crdt.track("room:lobby", make_pid(2), "user:2", vec![4, 5, 6]);
        crdt.track("room:other", make_pid(3), "user:3", vec![7, 8, 9]);

        let lobby = crdt.list("room:lobby");
        assert_eq!(lobby.len(), 2);
        assert!(lobby.contains_key("user:1"));
        assert!(lobby.contains_key("user:2"));

        let other = crdt.list("room:other");
        assert_eq!(other.len(), 1);
        assert!(other.contains_key("user:3"));
    }

    #[test]
    fn test_untrack_removes_entry() {
        let mut crdt = PresenceCrdt::new("node1");
        let pid = make_pid(1);

        crdt.track("room:lobby", pid, "user:1", vec![]);
        assert_eq!(crdt.list("room:lobby").len(), 1);

        let removed = crdt.untrack("room:lobby", "user:1", pid);
        assert_eq!(removed.len(), 1);
        assert_eq!(crdt.list("room:lobby").len(), 0);
    }

    #[test]
    fn test_untrack_all_removes_all_entries() {
        let mut crdt = PresenceCrdt::new("node1");
        let pid = make_pid(1);

        crdt.track("room:lobby", pid, "user:1", vec![]);
        crdt.track("room:other", pid, "user:1", vec![]);
        crdt.track("room:lobby", make_pid(2), "user:2", vec![]);

        let removed = crdt.untrack_all(pid);
        assert_eq!(removed.len(), 2); // 2 topics
        assert_eq!(crdt.list("room:lobby").len(), 1); // Only user:2 remains
        assert_eq!(crdt.list("room:other").len(), 0);
    }

    #[test]
    fn test_merge_adds_new_entries() {
        let mut crdt1 = PresenceCrdt::new("node1");
        let mut crdt2 = PresenceCrdt::new("node2");

        // Track on node1
        crdt1.track("room:lobby", make_pid(1), "user:1", vec![1]);

        // Track on node2
        crdt2.track("room:lobby", make_pid(2), "user:2", vec![2]);

        // Extract delta from node1
        let delta1 = crdt1.extract_all();

        // Merge into node2
        let result = crdt2.merge(&delta1);

        assert_eq!(result.joins.len(), 1);
        assert_eq!(result.leaves.len(), 0);

        // Node2 should now have both entries
        let lobby = crdt2.list("room:lobby");
        assert_eq!(lobby.len(), 2);
        assert!(lobby.contains_key("user:1"));
        assert!(lobby.contains_key("user:2"));
    }

    #[test]
    fn test_merge_ignores_duplicates() {
        let mut crdt1 = PresenceCrdt::new("node1");
        let mut crdt2 = PresenceCrdt::new("node2");

        // Track on node1
        crdt1.track("room:lobby", make_pid(1), "user:1", vec![1]);

        // Extract delta
        let delta = crdt1.extract_all();

        // Merge twice
        let result1 = crdt2.merge(&delta);
        let result2 = crdt2.merge(&delta);

        assert_eq!(result1.joins.len(), 1);
        assert_eq!(result2.joins.len(), 0); // No new joins on second merge

        // Should still only have 1 entry
        assert_eq!(crdt2.len(), 1);
    }

    #[test]
    fn test_merge_applies_leaves() {
        let mut crdt1 = PresenceCrdt::new("node1");
        let mut crdt2 = PresenceCrdt::new("node2");

        // Track on node1
        let pid = make_pid(1);
        crdt1.track("room:lobby", pid, "user:1", vec![]);

        // Sync to node2
        let delta1 = crdt1.extract_all();
        crdt2.merge(&delta1);
        assert_eq!(crdt2.len(), 1);

        // Untrack on node1
        crdt1.untrack("room:lobby", "user:1", pid);

        // Get delta with leave
        let delta2 = crdt1.take_delta().unwrap();
        assert_eq!(delta2.leave_tags.len(), 1);

        // Merge leave into node2
        let result = crdt2.merge(&delta2);
        assert_eq!(result.leaves.len(), 1);
        assert_eq!(crdt2.len(), 0);
    }

    #[test]
    fn test_extract_respects_remote_context() {
        let mut crdt = PresenceCrdt::new("node1");

        // Track multiple entries
        crdt.track("room:lobby", make_pid(1), "user:1", vec![]);
        crdt.track("room:lobby", make_pid(2), "user:2", vec![]);
        crdt.track("room:lobby", make_pid(3), "user:3", vec![]);

        // Remote has seen up to clock 2
        let remote_context = HashMap::from([("node1".to_string(), 2)]);

        // Extract should only return entry with clock 3
        let delta = crdt.extract(&remote_context);
        assert_eq!(delta.joins.len(), 1);
        assert_eq!(delta.joins[0].tag.clock, 3);
    }

    #[test]
    fn test_replica_up_down_filtering() {
        let mut crdt1 = PresenceCrdt::new("node1");
        let mut crdt2 = PresenceCrdt::new("node2");

        // Track on node1
        crdt1.track("room:lobby", make_pid(1), "user:1", vec![]);

        // Sync to node2
        let delta = crdt1.extract_all();
        crdt2.merge(&delta);

        // Both should be visible initially
        assert_eq!(crdt2.list("room:lobby").len(), 1);

        // Mark node1 as down
        let hidden = crdt2.replica_down("node1", true);
        assert_eq!(hidden.len(), 1);

        // Entry should be filtered out
        assert_eq!(crdt2.list("room:lobby").len(), 0);

        // Mark node1 as up again
        let visible = crdt2.replica_up("node1");
        assert_eq!(visible.len(), 1);

        // Entry should be visible again
        assert_eq!(crdt2.list("room:lobby").len(), 1);
    }

    #[test]
    fn test_concurrent_modifications_converge() {
        let mut crdt1 = PresenceCrdt::new("node1");
        let mut crdt2 = PresenceCrdt::new("node2");

        // Both nodes track entries concurrently
        crdt1.track("room:lobby", make_pid(1), "user:1", vec![]);
        crdt2.track("room:lobby", make_pid(2), "user:2", vec![]);

        // Exchange deltas
        let delta1 = crdt1.extract_all();
        let delta2 = crdt2.extract_all();

        crdt1.merge(&delta2);
        crdt2.merge(&delta1);

        // Both should have the same state
        assert_eq!(crdt1.len(), 2);
        assert_eq!(crdt2.len(), 2);

        let list1 = crdt1.list("room:lobby");
        let list2 = crdt2.list("room:lobby");

        assert_eq!(list1.len(), list2.len());
        assert!(list1.contains_key("user:1"));
        assert!(list1.contains_key("user:2"));
    }

    #[test]
    fn test_delta_accumulation() {
        let mut crdt = PresenceCrdt::new("node1");

        // No delta initially
        assert!(!crdt.has_delta());

        // Track creates delta
        crdt.track("room:lobby", make_pid(1), "user:1", vec![]);
        assert!(crdt.has_delta());

        // Take delta
        let delta = crdt.take_delta();
        assert!(delta.is_some());
        assert_eq!(delta.unwrap().joins.len(), 1);

        // Delta is now empty
        assert!(!crdt.has_delta());
        assert!(crdt.take_delta().is_none());
    }
}
