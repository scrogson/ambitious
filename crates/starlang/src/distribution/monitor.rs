//! Node monitoring for distribution.
//!
//! Allows processes to be notified when a remote node goes down.

use super::DIST_MANAGER;
use super::protocol::{DistError, DistMessage};
use crate::core::{Atom, Pid};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counter for generating unique monitor references.
static MONITOR_REF_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A reference to a node monitor.
///
/// Used to cancel monitoring with `demonitor_node`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeMonitorRef(u64);

impl NodeMonitorRef {
    /// Create a new unique monitor reference.
    fn new() -> Self {
        Self(MONITOR_REF_COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

/// Reason why a node went down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeDownReason {
    /// Normal disconnect.
    Disconnect,
    /// Connection lost unexpectedly.
    ConnectionLost,
    /// Node is shutting down.
    Shutdown,
    /// Custom reason.
    Other(String),
}

impl From<String> for NodeDownReason {
    fn from(s: String) -> Self {
        match s.as_str() {
            "disconnect" | "disconnect requested" => NodeDownReason::Disconnect,
            "shutdown" => NodeDownReason::Shutdown,
            "connection closed" | "connection lost" => NodeDownReason::ConnectionLost,
            _ => NodeDownReason::Other(s),
        }
    }
}

/// Message sent to a process when a monitored node goes down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDown {
    /// The node that went down (as a string for serialization).
    pub node_name: String,
    /// Why the node went down.
    pub reason: NodeDownReason,
    /// The monitor reference (for identifying which monitor triggered this).
    pub monitor_ref: u64,
}

/// Registry of node monitors.
pub struct NodeMonitorRegistry {
    /// Local monitors: node name atom -> set of (Pid, MonitorRef) pairs.
    local_monitors: DashMap<Atom, HashSet<(Pid, u64)>>,
    /// Reverse lookup: MonitorRef -> (node atom, Pid).
    monitor_refs: DashMap<u64, (Atom, Pid)>,
    /// Remote monitors: node name atom -> set of remote PIDs monitoring us.
    remote_monitors: DashMap<Atom, HashSet<Pid>>,
}

impl NodeMonitorRegistry {
    /// Create a new monitor registry.
    pub fn new() -> Self {
        Self {
            local_monitors: DashMap::new(),
            monitor_refs: DashMap::new(),
            remote_monitors: DashMap::new(),
        }
    }

    /// Add a local monitor for a node.
    ///
    /// The calling process will receive a `NodeDown` message if the node disconnects.
    pub fn add_local_monitor(&self, node_atom: Atom, pid: Pid) -> NodeMonitorRef {
        let monitor_ref = NodeMonitorRef::new();

        self.local_monitors
            .entry(node_atom)
            .or_default()
            .insert((pid, monitor_ref.0));

        self.monitor_refs.insert(monitor_ref.0, (node_atom, pid));

        monitor_ref
    }

    /// Remove a local monitor.
    pub fn remove_local_monitor(&self, monitor_ref: NodeMonitorRef) {
        if let Some((_, (node_atom, pid))) = self.monitor_refs.remove(&monitor_ref.0)
            && let Some(mut monitors) = self.local_monitors.get_mut(&node_atom) {
                monitors.remove(&(pid, monitor_ref.0));
            }
    }

    /// Add a remote process monitoring this node.
    pub fn add_remote_monitor(&self, from_node: Atom, pid: Pid) {
        self.remote_monitors
            .entry(from_node)
            .or_default()
            .insert(pid);
    }

    /// Remove a remote monitor.
    pub fn remove_remote_monitor(&self, from_node: Atom, pid: Pid) {
        if let Some(mut monitors) = self.remote_monitors.get_mut(&from_node) {
            monitors.remove(&pid);
        }
    }

    /// Notify all monitors that a node went down.
    pub fn notify_node_down(&self, node_atom: Atom, reason: String) {
        if let Some((_, monitors)) = self.local_monitors.remove(&node_atom) {
            for (pid, ref_id) in monitors {
                // Remove from reverse lookup
                self.monitor_refs.remove(&ref_id);

                // Send NodeDown message to the monitoring process
                let msg = NodeDown {
                    node_name: node_atom.as_str(),
                    reason: reason.clone().into(),
                    monitor_ref: ref_id,
                };

                if let Ok(payload) = postcard::to_allocvec(&msg)
                    && let Some(handle) = crate::process::global::try_handle() {
                        let _ = handle.registry().send_raw(pid, payload);
                    }
            }
        }

        // Also clean up any remote monitors from this node
        self.remote_monitors.remove(&node_atom);
    }
}

impl Default for NodeMonitorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// === Public API ===

/// Monitor a remote node.
///
/// The calling process will receive a `NodeDown` message if the node disconnects.
/// Returns a `NodeMonitorRef` that can be used to cancel the monitor.
///
/// # Example
///
/// ```ignore
/// let monitor_ref = starlang::dist::monitor_node(node_atom);
///
/// // Later, in handle_info:
/// if let Ok(node_down) = postcard::from_bytes::<NodeDown>(&msg) {
///     println!("Node {} went down: {:?}", node_down.node_name, node_down.reason);
/// }
/// ```
pub fn monitor_node(node_atom: Atom) -> Result<NodeMonitorRef, DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    let pid = crate::runtime::current_pid();

    let monitor_ref = manager.monitors().add_local_monitor(node_atom, pid);

    // Send MonitorNode message to remote
    if let Some(tx) = manager.get_node_tx(node_atom) {
        let msg = DistMessage::MonitorNode {
            requesting_pid: pid,
        };
        let _ = tx.try_send(msg);
    }

    Ok(monitor_ref)
}

/// Cancel a node monitor.
///
/// After calling this, you will no longer receive `NodeDown` messages for this monitor.
pub fn demonitor_node(monitor_ref: NodeMonitorRef) -> Result<(), DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;

    // Get the node atom before removing
    let node_atom = manager
        .monitors()
        .monitor_refs
        .get(&monitor_ref.0)
        .map(|r| r.0);

    manager.monitors().remove_local_monitor(monitor_ref);

    // Send DemonitorNode message to remote
    if let Some(node_atom) = node_atom
        && let Some(tx) = manager.get_node_tx(node_atom) {
            let msg = DistMessage::DemonitorNode {
                requesting_pid: crate::runtime::current_pid(),
            };
            let _ = tx.try_send(msg);
        }

    Ok(())
}
