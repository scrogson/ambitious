//! Distributed process groups for Starlang.
//!
//! Similar to Erlang's `pg` module (OTP 23+), this provides distributed
//! process groups where processes can join named groups and discover
//! other members across the cluster.
//!
//! # How it works
//!
//! - Processes can join/leave named groups
//! - Group membership is synchronized to all connected nodes
//! - `get_members/1` returns all members (local + remote)
//! - `get_local_members/1` returns only local members
//! - When a node disconnects, its members are automatically removed
//!
//! # Example
//!
//! ```ignore
//! use starlang::pg;
//!
//! // Join a group
//! pg::join("my_group", my_pid);
//!
//! // Get all members across the cluster
//! let members = pg::get_members("my_group");
//!
//! // Leave the group
//! pg::leave("my_group", my_pid);
//! ```

use super::DIST_MANAGER;
use super::protocol::DistMessage;
use crate::core::{Atom, Pid};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

/// Process groups instance.
static PG: OnceLock<ProcessGroups> = OnceLock::new();

/// Message types for process groups synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PgMessage {
    /// A process joined a group.
    Join {
        /// The group name.
        group: String,
        /// The process that joined.
        pid: Pid,
    },
    /// A process left a group.
    Leave {
        /// The group name.
        group: String,
        /// The process that left.
        pid: Pid,
    },
    /// A process left all groups (e.g., process died).
    LeaveAll {
        /// The process that left all groups.
        pid: Pid,
    },
    /// Request full sync (sent on connect).
    SyncRequest,
    /// Full sync response with all groups and their members.
    SyncResponse {
        /// All groups with their members.
        groups: Vec<(String, Vec<Pid>)>,
    },
}

/// The distributed process groups registry.
pub struct ProcessGroups {
    /// Group name -> set of member PIDs.
    groups: DashMap<String, HashSet<Pid>>,
    /// Reverse lookup: PID -> set of groups it belongs to.
    /// Used for efficient cleanup when a process leaves all groups.
    pid_to_groups: DashMap<Pid, HashSet<String>>,
}

impl ProcessGroups {
    /// Create a new process groups registry.
    pub fn new() -> Self {
        Self {
            groups: DashMap::new(),
            pid_to_groups: DashMap::new(),
        }
    }

    /// Join a process to a group.
    ///
    /// If this is a local process, the join is broadcast to all connected nodes.
    pub fn join(&self, group: impl Into<String>, pid: Pid) {
        let group = group.into();

        // Add to group
        self.groups.entry(group.clone()).or_default().insert(pid);

        // Track reverse mapping
        self.pid_to_groups
            .entry(pid)
            .or_default()
            .insert(group.clone());

        // Only broadcast if this is a local process joining
        if pid.is_local() {
            self.broadcast_join(&group, pid);
        }

        tracing::debug!(%group, ?pid, "Process joined group");
    }

    /// Remove a process from a group.
    ///
    /// If this is a local process, the leave is broadcast to all connected nodes.
    pub fn leave(&self, group: &str, pid: Pid) {
        // Remove from group
        if let Some(mut members) = self.groups.get_mut(group) {
            members.remove(&pid);
            // Clean up empty groups
            if members.is_empty() {
                drop(members);
                self.groups.remove(group);
            }
        }

        // Update reverse mapping
        if let Some(mut groups) = self.pid_to_groups.get_mut(&pid) {
            groups.remove(group);
            if groups.is_empty() {
                drop(groups);
                self.pid_to_groups.remove(&pid);
            }
        }

        // Only broadcast if this is a local process leaving
        if pid.is_local() {
            self.broadcast_leave(group, pid);
        }

        tracing::debug!(%group, ?pid, "Process left group");
    }

    /// Remove a process from all groups.
    ///
    /// This is typically called when a process terminates.
    pub fn leave_all(&self, pid: Pid) {
        if let Some((_, groups)) = self.pid_to_groups.remove(&pid) {
            for group in &groups {
                if let Some(mut members) = self.groups.get_mut(group) {
                    members.remove(&pid);
                    if members.is_empty() {
                        drop(members);
                        self.groups.remove(group);
                    }
                }
            }

            // Only broadcast if this is a local process
            if pid.is_local() {
                self.broadcast_leave_all(pid);
            }

            tracing::debug!(?pid, groups = ?groups, "Process left all groups");
        }
    }

    /// Get all members of a group (local + remote).
    pub fn get_members(&self, group: &str) -> Vec<Pid> {
        self.groups
            .get(group)
            .map(|members| members.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Get only local members of a group.
    pub fn get_local_members(&self, group: &str) -> Vec<Pid> {
        self.groups
            .get(group)
            .map(|members| members.iter().filter(|p| p.is_local()).copied().collect())
            .unwrap_or_default()
    }

    /// Get all groups that a process belongs to.
    pub fn which_groups(&self, pid: Pid) -> Vec<String> {
        self.pid_to_groups
            .get(&pid)
            .map(|groups| groups.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all group names.
    pub fn all_groups(&self) -> Vec<String> {
        self.groups.iter().map(|r| r.key().clone()).collect()
    }

    /// Handle an incoming pg message from another node.
    pub fn handle_message(&self, msg: PgMessage, from_node: Atom) {
        match msg {
            PgMessage::Join { group, pid } => {
                tracing::debug!(%group, ?pid, from_node = %from_node, "Received pg join");
                // Don't broadcast - this is already a remote join
                self.groups.entry(group.clone()).or_default().insert(pid);
                self.pid_to_groups.entry(pid).or_default().insert(group);
            }
            PgMessage::Leave { group, pid } => {
                tracing::debug!(%group, ?pid, from_node = %from_node, "Received pg leave");
                if let Some(mut members) = self.groups.get_mut(&group) {
                    members.remove(&pid);
                    if members.is_empty() {
                        drop(members);
                        self.groups.remove(&group);
                    }
                }
                if let Some(mut groups) = self.pid_to_groups.get_mut(&pid) {
                    groups.remove(&group);
                    if groups.is_empty() {
                        drop(groups);
                        self.pid_to_groups.remove(&pid);
                    }
                }
            }
            PgMessage::LeaveAll { pid } => {
                tracing::debug!(?pid, from_node = %from_node, "Received pg leave_all");
                if let Some((_, groups)) = self.pid_to_groups.remove(&pid) {
                    for group in groups {
                        if let Some(mut members) = self.groups.get_mut(&group) {
                            members.remove(&pid);
                            if members.is_empty() {
                                drop(members);
                                self.groups.remove(&group);
                            }
                        }
                    }
                }
            }
            PgMessage::SyncRequest => {
                tracing::debug!(from_node = %from_node, "Received pg sync request");
                // Send our local members to the requesting node
                let groups: Vec<(String, Vec<Pid>)> = self
                    .groups
                    .iter()
                    .map(|r| {
                        let local_members: Vec<Pid> =
                            r.value().iter().filter(|p| p.is_local()).copied().collect();
                        (r.key().clone(), local_members)
                    })
                    .filter(|(_, members)| !members.is_empty())
                    .collect();

                if let Some(manager) = DIST_MANAGER.get()
                    && let Some(tx) = manager.get_node_tx(from_node) {
                        let msg = PgMessage::SyncResponse { groups };
                        if let Ok(payload) = postcard::to_allocvec(&msg) {
                            let _ = tx.try_send(DistMessage::ProcessGroups { payload });
                        }
                    }
            }
            PgMessage::SyncResponse { groups } => {
                tracing::debug!(
                    count = groups.len(),
                    from_node = %from_node,
                    "Received pg sync response"
                );
                // Merge remote group memberships
                for (group, members) in groups {
                    for pid in members {
                        self.groups.entry(group.clone()).or_default().insert(pid);
                        self.pid_to_groups
                            .entry(pid)
                            .or_default()
                            .insert(group.clone());
                    }
                }
            }
        }
    }

    /// Remove all members from a specific node.
    ///
    /// Called when a node disconnects.
    pub fn remove_node_members(&self, node_atom: Atom) {
        let node_name = node_atom.as_str();

        // Find all PIDs from this node
        let pids_to_remove: Vec<Pid> = self
            .pid_to_groups
            .iter()
            .filter(|r| r.key().node_name() == node_name)
            .map(|r| *r.key())
            .collect();

        for pid in pids_to_remove {
            if let Some((_, groups)) = self.pid_to_groups.remove(&pid) {
                for group in groups {
                    if let Some(mut members) = self.groups.get_mut(&group) {
                        members.remove(&pid);
                        if members.is_empty() {
                            drop(members);
                            self.groups.remove(&group);
                        }
                    }
                }
            }
        }

        tracing::debug!(node = %node_atom, "Removed all members from disconnected node");
    }

    /// Request sync from a newly connected node.
    pub fn request_sync(&self, node_atom: Atom) {
        if let Some(manager) = DIST_MANAGER.get()
            && let Some(tx) = manager.get_node_tx(node_atom) {
                // Ask them to send their group memberships to us
                let msg = PgMessage::SyncRequest;
                if let Ok(payload) = postcard::to_allocvec(&msg) {
                    let _ = tx.try_send(DistMessage::ProcessGroups { payload });
                }

                // Also push our local memberships to them
                let groups: Vec<(String, Vec<Pid>)> = self
                    .groups
                    .iter()
                    .map(|r| {
                        let local_members: Vec<Pid> =
                            r.value().iter().filter(|p| p.is_local()).copied().collect();
                        (r.key().clone(), local_members)
                    })
                    .filter(|(_, members)| !members.is_empty())
                    .collect();

                if !groups.is_empty() {
                    let response = PgMessage::SyncResponse { groups };
                    if let Ok(payload) = postcard::to_allocvec(&response) {
                        let _ = tx.try_send(DistMessage::ProcessGroups { payload });
                    }
                }
            }
    }

    /// Broadcast a join to all connected nodes.
    fn broadcast_join(&self, group: &str, pid: Pid) {
        let msg = PgMessage::Join {
            group: group.to_string(),
            pid,
        };
        self.broadcast_pg_message(&msg);
    }

    /// Broadcast a leave to all connected nodes.
    fn broadcast_leave(&self, group: &str, pid: Pid) {
        let msg = PgMessage::Leave {
            group: group.to_string(),
            pid,
        };
        self.broadcast_pg_message(&msg);
    }

    /// Broadcast a leave_all to all connected nodes.
    fn broadcast_leave_all(&self, pid: Pid) {
        let msg = PgMessage::LeaveAll { pid };
        self.broadcast_pg_message(&msg);
    }

    /// Broadcast a pg message to all connected nodes.
    fn broadcast_pg_message(&self, msg: &PgMessage) {
        if let Some(manager) = DIST_MANAGER.get()
            && let Ok(payload) = postcard::to_allocvec(msg) {
                let dist_msg = DistMessage::ProcessGroups { payload };
                for node_atom in manager.connected_nodes() {
                    if let Some(tx) = manager.get_node_tx(node_atom) {
                        let _ = tx.try_send(dist_msg.clone());
                    }
                }
            }
    }
}

impl Default for ProcessGroups {
    fn default() -> Self {
        Self::new()
    }
}

/// Get or initialize the process groups registry.
pub fn pg() -> &'static ProcessGroups {
    PG.get_or_init(ProcessGroups::new)
}

// === Public API ===

/// Join a process to a group.
///
/// The membership will be synchronized to all connected nodes.
///
/// # Example
///
/// ```ignore
/// use starlang::pg;
///
/// pg::join("workers", my_pid);
/// ```
pub fn join(group: impl Into<String>, pid: Pid) {
    pg().join(group, pid);
}

/// Remove a process from a group.
///
/// # Example
///
/// ```ignore
/// use starlang::pg;
///
/// pg::leave("workers", my_pid);
/// ```
pub fn leave(group: &str, pid: Pid) {
    pg().leave(group, pid);
}

/// Remove a process from all groups.
///
/// This should be called when a process terminates.
pub fn leave_all(pid: Pid) {
    pg().leave_all(pid);
}

/// Get all members of a group (local + remote).
///
/// # Example
///
/// ```ignore
/// use starlang::pg;
///
/// for member in pg::get_members("workers") {
///     starlang::send(member, work_message);
/// }
/// ```
pub fn get_members(group: &str) -> Vec<Pid> {
    pg().get_members(group)
}

/// Get only local members of a group.
///
/// This is useful when you only want to communicate with processes
/// on the same node.
pub fn get_local_members(group: &str) -> Vec<Pid> {
    pg().get_local_members(group)
}

/// Get all groups that a process belongs to.
pub fn which_groups(pid: Pid) -> Vec<String> {
    pg().which_groups(pid)
}

/// Get all group names.
pub fn all_groups() -> Vec<String> {
    pg().all_groups()
}
