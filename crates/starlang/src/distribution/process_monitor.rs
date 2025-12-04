//! Remote process monitoring and linking.
//!
//! Enables monitoring and linking processes across node boundaries.
//! When a local process monitors/links a remote process (or vice versa),
//! the appropriate DOWN/EXIT messages are delivered when the process dies.

use super::DIST_MANAGER;
use super::protocol::{DistError, DistMessage};
use crate::core::{Atom, ExitReason, Pid, Ref, SystemMessage};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Registry for remote process monitors and links.
pub struct ProcessMonitorRegistry {
    /// Remote monitors we're tracking.
    /// Maps (monitored_pid, reference) -> monitoring_pid
    /// These are monitors where a LOCAL process is monitoring a REMOTE process.
    outgoing_monitors: DashMap<(Pid, Ref), Pid>,

    /// Incoming monitors from remote processes.
    /// Maps local_pid -> set of (remote_monitor_pid, reference)
    /// These are monitors where a REMOTE process is monitoring a LOCAL process.
    incoming_monitors: DashMap<Pid, HashSet<(Pid, Ref)>>,

    /// Remote links we're tracking.
    /// Maps local_pid -> set of remote_pids linked to it.
    links: DashMap<Pid, HashSet<Pid>>,

    /// Track which remote monitors/links belong to which node
    /// for cleanup when a node disconnects.
    /// node -> (monitors: Vec<(local_pid, remote_pid, ref)>, links: Vec<(local_pid, remote_pid)>)
    node_tracking: RwLock<HashMap<Atom, NodeTracking>>,
}

#[derive(Default)]
struct NodeTracking {
    /// Outgoing monitors to this node: (local_monitor_pid, remote_target_pid, ref)
    outgoing_monitors: Vec<(Pid, Pid, Ref)>,
    /// Incoming monitors from this node: (local_target_pid, remote_monitor_pid, ref)
    incoming_monitors: Vec<(Pid, Pid, Ref)>,
    /// Links involving this node: (local_pid, remote_pid)
    links: Vec<(Pid, Pid)>,
}

impl ProcessMonitorRegistry {
    /// Create a new process monitor registry.
    pub fn new() -> Self {
        Self {
            outgoing_monitors: DashMap::new(),
            incoming_monitors: DashMap::new(),
            links: DashMap::new(),
            node_tracking: RwLock::new(HashMap::new()),
        }
    }

    // =========================================================================
    // Outgoing monitors (local process monitoring remote process)
    // =========================================================================

    /// Add an outgoing monitor (local process monitoring a remote process).
    pub fn add_outgoing_monitor(&self, monitor_pid: Pid, target_pid: Pid, reference: Ref) {
        self.outgoing_monitors
            .insert((target_pid, reference), monitor_pid);

        // Track for node cleanup
        let node = target_pid.node();
        let mut tracking = self.node_tracking.write().unwrap();
        tracking.entry(node).or_default().outgoing_monitors.push((
            monitor_pid,
            target_pid,
            reference,
        ));
    }

    /// Remove an outgoing monitor.
    pub fn remove_outgoing_monitor(&self, target_pid: Pid, reference: Ref) {
        self.outgoing_monitors.remove(&(target_pid, reference));
    }

    /// Handle a ProcessDown message from a remote node.
    /// Delivers the DOWN message to the local monitoring process.
    pub fn handle_process_down(&self, reference: Ref, pid: Pid, reason: &str) {
        // Find and remove the monitor
        if let Some((_, monitor_pid)) = self.outgoing_monitors.remove(&(pid, reference)) {
            // Deliver DOWN message to the monitoring process
            let exit_reason: ExitReason = reason.into();
            let down_msg = SystemMessage::down(reference, pid, exit_reason);

            if let Some(handle) = crate::process::global::try_handle() {
                let _ = handle.registry().send(monitor_pid, &down_msg);
            }
        }
    }

    // =========================================================================
    // Incoming monitors (remote process monitoring local process)
    // =========================================================================

    /// Add an incoming monitor (remote process monitoring a local process).
    pub fn add_incoming_monitor(
        &self,
        target_pid: Pid,
        monitor_pid: Pid,
        reference: Ref,
        from_node: Atom,
    ) {
        self.incoming_monitors
            .entry(target_pid)
            .or_default()
            .insert((monitor_pid, reference));

        // Track for node cleanup
        let mut tracking = self.node_tracking.write().unwrap();
        tracking
            .entry(from_node)
            .or_default()
            .incoming_monitors
            .push((target_pid, monitor_pid, reference));
    }

    /// Remove an incoming monitor.
    pub fn remove_incoming_monitor(&self, target_pid: Pid, monitor_pid: Pid, reference: Ref) {
        if let Some(mut monitors) = self.incoming_monitors.get_mut(&target_pid) {
            monitors.remove(&(monitor_pid, reference));
        }
    }

    /// Called when a local process dies - notify remote monitors.
    pub fn notify_local_process_down(&self, pid: Pid, reason: &ExitReason) {
        if let Some((_, monitors)) = self.incoming_monitors.remove(&pid) {
            for (monitor_pid, reference) in monitors {
                // Send ProcessDown to the remote node
                let node = monitor_pid.node();
                if let Some(manager) = DIST_MANAGER.get()
                    && let Some(tx) = manager.get_node_tx(node)
                {
                    let msg = DistMessage::ProcessDown {
                        reference: reference.as_raw(),
                        pid,
                        reason: reason.to_string(),
                    };
                    let _ = tx.try_send(msg);
                }
            }
        }

        // Also clean up any outgoing monitors from this process
        // (The remote side will handle its own cleanup when the connection sees this)
        self.outgoing_monitors
            .retain(|(target, _ref), monitor| *monitor != pid || *target == pid);

        // Clean up links
        self.handle_local_process_exit_links(pid, reason);
    }

    // =========================================================================
    // Links
    // =========================================================================

    /// Add a bidirectional link between a local and remote process.
    pub fn add_link(&self, local_pid: Pid, remote_pid: Pid) {
        self.links.entry(local_pid).or_default().insert(remote_pid);

        // Track for node cleanup
        let node = remote_pid.node();
        let mut tracking = self.node_tracking.write().unwrap();
        tracking
            .entry(node)
            .or_default()
            .links
            .push((local_pid, remote_pid));
    }

    /// Remove a link.
    pub fn remove_link(&self, local_pid: Pid, remote_pid: Pid) {
        if let Some(mut linked) = self.links.get_mut(&local_pid) {
            linked.remove(&remote_pid);
        }
    }

    /// Handle incoming link request from remote.
    pub fn handle_incoming_link(&self, from_pid: Pid, target_pid: Pid, from_node: Atom) {
        // Check if target is alive
        if let Some(handle) = crate::process::global::try_handle() {
            if handle.alive(target_pid) {
                // Add to our link tracking
                self.add_link(target_pid, from_pid);

                // Register the link with the local process
                if let Some(proc_handle) = handle.registry().get(target_pid) {
                    proc_handle.add_remote_link(from_pid);
                }
            } else {
                // Target is dead - send exit signal back
                if let Some(manager) = DIST_MANAGER.get()
                    && let Some(tx) = manager.get_node_tx(from_node)
                {
                    let msg = DistMessage::Exit {
                        from: target_pid,
                        target: from_pid,
                        reason: "noproc".to_string(),
                    };
                    let _ = tx.try_send(msg);
                }
            }
        }
    }

    /// Handle incoming unlink request from remote.
    pub fn handle_incoming_unlink(&self, from_pid: Pid, target_pid: Pid) {
        self.remove_link(target_pid, from_pid);

        // Remove from local process
        if let Some(handle) = crate::process::global::try_handle()
            && let Some(proc_handle) = handle.registry().get(target_pid)
        {
            proc_handle.remove_remote_link(from_pid);
        }
    }

    /// Handle incoming exit signal from remote linked process.
    pub fn handle_incoming_exit(&self, from_pid: Pid, target_pid: Pid, reason: &str) {
        // Remove the link
        self.remove_link(target_pid, from_pid);

        // Deliver exit signal to local process
        if let Some(handle) = crate::process::global::try_handle()
            && let Some(proc_handle) = handle.registry().get(target_pid)
        {
            proc_handle.remove_remote_link(from_pid);

            let exit_reason: ExitReason = reason.into();
            if exit_reason.is_killed() {
                // Killed propagates unconditionally
                proc_handle.mark_terminated(ExitReason::Killed);
            } else if proc_handle.is_trapping_exits() {
                // Send as message
                let exit_msg = SystemMessage::exit(from_pid, exit_reason);
                let _ = proc_handle.send(&exit_msg);
            } else if exit_reason.is_abnormal() {
                // Propagate abnormal exit
                proc_handle.mark_terminated(exit_reason);
            }
            // Normal exits don't propagate to non-trapping processes
        }
    }

    /// Called when a local process dies - notify remote linked processes.
    fn handle_local_process_exit_links(&self, pid: Pid, reason: &ExitReason) {
        if let Some((_, linked_pids)) = self.links.remove(&pid) {
            for remote_pid in linked_pids {
                let node = remote_pid.node();
                if let Some(manager) = DIST_MANAGER.get()
                    && let Some(tx) = manager.get_node_tx(node)
                {
                    let msg = DistMessage::Exit {
                        from: pid,
                        target: remote_pid,
                        reason: reason.to_string(),
                    };
                    let _ = tx.try_send(msg);
                }
            }
        }
    }

    // =========================================================================
    // Node cleanup
    // =========================================================================

    /// Clean up all monitors and links when a node disconnects.
    pub fn handle_node_down(&self, node: Atom) {
        let tracking = {
            let mut tracking = self.node_tracking.write().unwrap();
            tracking.remove(&node)
        };

        let Some(tracking) = tracking else {
            return;
        };

        // Handle outgoing monitors - deliver DOWN to local processes
        for (monitor_pid, target_pid, reference) in tracking.outgoing_monitors {
            self.outgoing_monitors.remove(&(target_pid, reference));

            // Deliver DOWN message with "noconnection" reason
            let down_msg =
                SystemMessage::down(reference, target_pid, ExitReason::error("noconnection"));

            if let Some(handle) = crate::process::global::try_handle() {
                let _ = handle.registry().send(monitor_pid, &down_msg);
            }
        }

        // Handle incoming monitors - just clean up, remote node is gone
        for (target_pid, monitor_pid, _reference) in tracking.incoming_monitors {
            if let Some(mut monitors) = self.incoming_monitors.get_mut(&target_pid) {
                monitors.retain(|(pid, _)| *pid != monitor_pid);
            }
        }

        // Handle links - deliver exit signals to local processes
        for (local_pid, remote_pid) in tracking.links {
            if let Some(mut linked) = self.links.get_mut(&local_pid) {
                linked.remove(&remote_pid);
            }

            // Deliver exit signal
            if let Some(handle) = crate::process::global::try_handle()
                && let Some(proc_handle) = handle.registry().get(local_pid)
            {
                proc_handle.remove_remote_link(remote_pid);

                if proc_handle.is_trapping_exits() {
                    let exit_msg =
                        SystemMessage::exit(remote_pid, ExitReason::error("noconnection"));
                    let _ = proc_handle.send(&exit_msg);
                } else {
                    // Propagate as abnormal exit
                    proc_handle.mark_terminated(ExitReason::error("noconnection"));
                }
            }
        }
    }
}

impl Default for ProcessMonitorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Monitor a remote process.
///
/// The calling process will receive a `SystemMessage::Down` when the
/// remote process exits.
///
/// # Example
///
/// ```ignore
/// let reference = starlang::dist::monitor_process(remote_pid)?;
///
/// // Later, handle the DOWN message:
/// if let Ok(SystemMessage::Down { reference, pid, reason }) = msg.decode() {
///     println!("Process {:?} exited: {:?}", pid, reason);
/// }
/// ```
pub fn monitor_process(target_pid: Pid) -> Result<Ref, DistError> {
    if target_pid.is_local() {
        return Err(DistError::InvalidAddress(
            "Cannot use dist::monitor_process for local process".to_string(),
        ));
    }

    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    let monitor_pid = crate::runtime::current_pid();
    let reference = Ref::new();
    let node = target_pid.node();

    // Register locally
    manager
        .process_monitors()
        .add_outgoing_monitor(monitor_pid, target_pid, reference);

    // Send MonitorProcess to remote node
    if let Some(tx) = manager.get_node_tx(node) {
        let msg = DistMessage::MonitorProcess {
            from: monitor_pid,
            target: target_pid,
            reference: reference.as_raw(),
        };
        let _ = tx.try_send(msg);
    } else {
        // Node not connected - deliver DOWN immediately
        manager
            .process_monitors()
            .remove_outgoing_monitor(target_pid, reference);
        return Err(DistError::NotConnected(node));
    }

    Ok(reference)
}

/// Cancel a remote process monitor.
pub fn demonitor_process(target_pid: Pid, reference: Ref) -> Result<(), DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    let from_pid = crate::runtime::current_pid();
    let node = target_pid.node();

    // Remove locally
    manager
        .process_monitors()
        .remove_outgoing_monitor(target_pid, reference);

    // Send DemonitorProcess to remote node
    if let Some(tx) = manager.get_node_tx(node) {
        let msg = DistMessage::DemonitorProcess {
            from: from_pid,
            target: target_pid,
            reference: reference.as_raw(),
        };
        let _ = tx.try_send(msg);
    }

    Ok(())
}

/// Link the current process to a remote process.
///
/// Creates a bidirectional link. If either process exits abnormally,
/// the other will receive an exit signal.
pub fn link_process(remote_pid: Pid) -> Result<(), DistError> {
    if remote_pid.is_local() {
        return Err(DistError::InvalidAddress(
            "Cannot use dist::link_process for local process".to_string(),
        ));
    }

    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    let local_pid = crate::runtime::current_pid();
    let node = remote_pid.node();

    // Register locally
    manager.process_monitors().add_link(local_pid, remote_pid);

    // Also register on the local process handle
    if let Some(handle) = crate::process::global::try_handle()
        && let Some(proc_handle) = handle.registry().get(local_pid)
    {
        proc_handle.add_remote_link(remote_pid);
    }

    // Send Link to remote node
    if let Some(tx) = manager.get_node_tx(node) {
        let msg = DistMessage::Link {
            from: local_pid,
            target: remote_pid,
        };
        let _ = tx.try_send(msg);
    } else {
        // Node not connected - clean up and return error
        manager
            .process_monitors()
            .remove_link(local_pid, remote_pid);
        return Err(DistError::NotConnected(node));
    }

    Ok(())
}

/// Remove a link to a remote process.
pub fn unlink_process(remote_pid: Pid) -> Result<(), DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    let local_pid = crate::runtime::current_pid();
    let node = remote_pid.node();

    // Remove locally
    manager
        .process_monitors()
        .remove_link(local_pid, remote_pid);

    // Remove from local process handle
    if let Some(handle) = crate::process::global::try_handle()
        && let Some(proc_handle) = handle.registry().get(local_pid)
    {
        proc_handle.remove_remote_link(remote_pid);
    }

    // Send Unlink to remote node
    if let Some(tx) = manager.get_node_tx(node) {
        let msg = DistMessage::Unlink {
            from: local_pid,
            target: remote_pid,
        };
        let _ = tx.try_send(msg);
    }

    Ok(())
}
