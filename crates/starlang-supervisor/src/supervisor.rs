//! Supervisor implementation.
//!
//! The supervisor manages child processes according to a supervision strategy.

use crate::error::{DeleteError, StartError, TerminateError};
use crate::types::{
    ChildCounts, ChildInfo, ChildSpec, ChildType, RestartType, Strategy, SupervisorFlags,
};
use starlang_core::{ExitReason, Pid, Ref, SystemMessage, Term};
use starlang_process::RuntimeHandle;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The Supervisor trait for implementing supervision trees.
///
/// Supervisors manage child processes and handle their failures according
/// to a configurable strategy.
///
/// # Example
///
/// ```ignore
/// use starlang_supervisor::{Supervisor, SupervisorInit, SupervisorFlags, ChildSpec, Strategy};
///
/// struct MySupervisor;
///
/// impl Supervisor for MySupervisor {
///     fn init(_arg: ()) -> SupervisorInit {
///         SupervisorInit {
///             flags: SupervisorFlags::new(Strategy::OneForOne),
///             children: vec![
///                 ChildSpec::new("worker1", || async { /* start worker */ }),
///             ],
///         }
///     }
/// }
/// ```
pub trait Supervisor: Sized + Send + 'static {
    /// Initializes the supervisor with flags and child specifications.
    fn init(arg: Self::InitArg) -> SupervisorInit;

    /// The type of argument passed to init.
    type InitArg: Send + 'static;
}

/// Result of supervisor initialization.
pub struct SupervisorInit {
    /// Supervisor configuration flags.
    pub flags: SupervisorFlags,
    /// Initial child specifications.
    pub children: Vec<ChildSpec>,
}

impl SupervisorInit {
    /// Creates a new supervisor init result.
    pub fn new(flags: SupervisorFlags, children: Vec<ChildSpec>) -> Self {
        Self { flags, children }
    }
}

/// Internal state of a running child.
struct ChildState {
    /// The child specification.
    spec: ChildSpec,
    /// The child's PID if running.
    pid: Option<Pid>,
    /// Monitor reference if monitoring.
    monitor_ref: Option<Ref>,
}

/// Internal supervisor state.
struct SupervisorState {
    /// The runtime handle for spawning (reserved for future use).
    #[allow(dead_code)]
    handle: RuntimeHandle,
    /// The supervisor's own PID (reserved for future use).
    #[allow(dead_code)]
    self_pid: Pid,
    /// Supervisor flags.
    flags: SupervisorFlags,
    /// Children indexed by ID.
    children: HashMap<String, ChildState>,
    /// Order of children (for RestForOne).
    child_order: Vec<String>,
    /// PID to child ID mapping.
    pid_to_id: HashMap<Pid, String>,
    /// Restart history for rate limiting.
    restart_times: Vec<Instant>,
}

impl SupervisorState {
    /// Starts a child and monitors it.
    async fn start_child(&mut self, id: &str) -> Result<Pid, String> {
        let child = self
            .children
            .get(id)
            .ok_or_else(|| format!("child '{}' not found", id))?;

        // Call the start function
        let pid = (child.spec.start)().await.map_err(|e| format!("{}", e))?;

        // Monitor the child using task-local context
        let monitor_ref =
            starlang_runtime::with_ctx(|ctx| ctx.monitor(pid)).map_err(|e| format!("{}", e))?;

        // Update state
        if let Some(child) = self.children.get_mut(id) {
            child.pid = Some(pid);
            child.monitor_ref = Some(monitor_ref);
        }
        self.pid_to_id.insert(pid, id.to_string());

        Ok(pid)
    }

    /// Handles a child exit.
    async fn handle_child_exit(&mut self, pid: Pid, reason: ExitReason) -> Result<(), ExitReason> {
        let id = match self.pid_to_id.remove(&pid) {
            Some(id) => id,
            None => return Ok(()), // Not our child
        };

        let should_restart = {
            let child = match self.children.get_mut(&id) {
                Some(c) => c,
                None => return Ok(()),
            };

            child.pid = None;
            child.monitor_ref = None;

            match child.spec.restart {
                RestartType::Permanent => true,
                RestartType::Transient => reason.is_abnormal(),
                RestartType::Temporary => false,
            }
        };

        if !should_restart {
            return Ok(());
        }

        // Check restart rate
        let now = Instant::now();
        let cutoff = now - Duration::from_secs(self.flags.max_seconds as u64);
        self.restart_times.retain(|t| *t > cutoff);

        if self.restart_times.len() >= self.flags.max_restarts as usize {
            return Err(ExitReason::error("max restart intensity reached"));
        }

        self.restart_times.push(now);

        // Handle strategy
        match self.flags.strategy {
            Strategy::OneForOne => {
                // Just restart this child
                if let Err(e) = self.start_child(&id).await {
                    // Child failed to restart
                    return Err(ExitReason::error(format!(
                        "child '{}' failed to restart: {}",
                        id, e
                    )));
                }
            }
            Strategy::OneForAll => {
                // Terminate all children, then restart all
                self.terminate_all_children().await;
                self.start_all_children().await?;
            }
            Strategy::RestForOne => {
                // Find position of failed child
                let pos = self.child_order.iter().position(|i| i == &id).unwrap_or(0);

                // Collect child IDs to terminate (from pos onwards, in reverse order)
                let to_terminate: Vec<String> =
                    self.child_order[pos..].iter().rev().cloned().collect();
                for child_id in to_terminate {
                    self.terminate_child_by_id(&child_id).await;
                }

                // Collect child IDs to restart (from pos onwards)
                let to_restart: Vec<String> = self.child_order[pos..].to_vec();
                for child_id in to_restart {
                    if let Err(e) = self.start_child(&child_id).await {
                        return Err(ExitReason::error(format!(
                            "child '{}' failed to restart: {}",
                            child_id, e
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Terminates all children in reverse order.
    async fn terminate_all_children(&mut self) {
        let ids: Vec<String> = self.child_order.iter().rev().cloned().collect();
        for id in ids {
            self.terminate_child_by_id(&id).await;
        }
    }

    /// Terminates a specific child by ID.
    async fn terminate_child_by_id(&mut self, id: &str) {
        if let Some(child) = self.children.get_mut(id) {
            if let Some(pid) = child.pid.take() {
                self.pid_to_id.remove(&pid);

                // Demonitor first using task-local context
                if let Some(ref_) = child.monitor_ref.take() {
                    starlang_runtime::with_ctx(|ctx| ctx.demonitor(ref_));
                }

                // Send exit signal using task-local context
                let _ = starlang_runtime::with_ctx(|ctx| ctx.exit(pid, ExitReason::Shutdown));

                // TODO: Wait for child to actually terminate with timeout
            }
        }
    }

    /// Starts all children in order.
    async fn start_all_children(&mut self) -> Result<(), ExitReason> {
        for id in self.child_order.clone() {
            if let Err(e) = self.start_child(&id).await {
                return Err(ExitReason::error(format!(
                    "child '{}' failed to start: {}",
                    id, e
                )));
            }
        }
        Ok(())
    }

    /// Gets counts of children (reserved for future use).
    #[allow(dead_code)]
    fn count_children(&self) -> ChildCounts {
        let mut counts = ChildCounts {
            specs: self.children.len(),
            active: 0,
            supervisors: 0,
            workers: 0,
        };

        for child in self.children.values() {
            if child.pid.is_some() {
                counts.active += 1;
                match child.spec.child_type {
                    ChildType::Supervisor => counts.supervisors += 1,
                    ChildType::Worker => counts.workers += 1,
                }
            }
        }

        counts
    }

    /// Gets information about all children (reserved for future use).
    #[allow(dead_code)]
    fn which_children(&self) -> Vec<ChildInfo> {
        self.children
            .values()
            .map(|c| ChildInfo {
                id: c.spec.id.clone(),
                pid: c.pid,
                child_type: c.spec.child_type,
            })
            .collect()
    }
}

/// The main supervisor process loop.
async fn supervisor_loop(mut state: SupervisorState) {
    // Start all initial children
    if let Err(_reason) = state.start_all_children().await {
        // Failed to start children - supervisor terminates
        return;
    }

    // Main message loop
    loop {
        let msg = match starlang_runtime::recv().await {
            Some(m) => m,
            None => {
                // Mailbox closed - terminate children and exit
                state.terminate_all_children().await;
                return;
            }
        };

        // Check for DOWN messages
        if let Ok(SystemMessage::Down {
            monitor_ref: _,
            pid,
            reason,
        }) = <SystemMessage as Term>::decode(&msg)
        {
            if let Err(_exit_reason) = state.handle_child_exit(pid, reason).await {
                // Supervisor needs to stop due to restart intensity
                state.terminate_all_children().await;
                return;
            }
        }

        // Check for exit signals
        if let Ok(SystemMessage::Exit { from: _, reason: _ }) =
            <SystemMessage as Term>::decode(&msg)
        {
            // If we receive an exit signal, terminate
            state.terminate_all_children().await;
            return;
        }
    }
}

/// Starts a supervisor with the given implementation.
///
/// Returns the PID of the started supervisor.
pub async fn start_link<S: Supervisor>(
    handle: &RuntimeHandle,
    parent: Pid,
    arg: S::InitArg,
) -> Result<Pid, StartError> {
    let init_result = S::init(arg);

    let handle_clone = handle.clone();
    let pid = handle.spawn_link(parent, move || {
        let self_pid = starlang_runtime::current_pid();

        // Set up trap_exit so we get exit signals as messages
        starlang_runtime::with_ctx(|ctx| ctx.set_trap_exit(true));

        let mut children = HashMap::new();
        let mut child_order = Vec::new();

        for spec in init_result.children {
            let id = spec.id.clone();
            children.insert(
                id.clone(),
                ChildState {
                    spec,
                    pid: None,
                    monitor_ref: None,
                },
            );
            child_order.push(id);
        }

        let state = SupervisorState {
            handle: handle_clone,
            self_pid,
            flags: init_result.flags,
            children,
            child_order,
            pid_to_id: HashMap::new(),
            restart_times: Vec::new(),
        };

        supervisor_loop(state)
    });

    // Give the supervisor time to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    if handle.alive(pid) {
        Ok(pid)
    } else {
        Err(StartError::InitFailed(
            "supervisor died during init".to_string(),
        ))
    }
}

/// Starts a supervisor without linking.
pub async fn start<S: Supervisor>(
    handle: &RuntimeHandle,
    arg: S::InitArg,
) -> Result<Pid, StartError> {
    let init_result = S::init(arg);

    let handle_clone = handle.clone();
    let pid = handle.spawn(move || {
        let self_pid = starlang_runtime::current_pid();

        // Set up trap_exit so we get exit signals as messages
        starlang_runtime::with_ctx(|ctx| ctx.set_trap_exit(true));

        let mut children = HashMap::new();
        let mut child_order = Vec::new();

        for spec in init_result.children {
            let id = spec.id.clone();
            children.insert(
                id.clone(),
                ChildState {
                    spec,
                    pid: None,
                    monitor_ref: None,
                },
            );
            child_order.push(id);
        }

        let state = SupervisorState {
            handle: handle_clone,
            self_pid,
            flags: init_result.flags,
            children,
            child_order,
            pid_to_id: HashMap::new(),
            restart_times: Vec::new(),
        };

        supervisor_loop(state)
    });

    // Give the supervisor time to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    if handle.alive(pid) {
        Ok(pid)
    } else {
        Err(StartError::InitFailed(
            "supervisor died during init".to_string(),
        ))
    }
}

/// Gets information about all children of a supervisor.
///
/// Note: This is a simplified implementation. A full implementation would
/// use a call/response protocol to query the supervisor.
pub async fn which_children(_handle: &RuntimeHandle, _sup: Pid) -> Result<Vec<ChildInfo>, String> {
    // This would need a proper call mechanism
    // For now, return an error indicating this needs more work
    Err("which_children not yet implemented".to_string())
}

/// Gets counts of supervisor children.
///
/// Note: This is a simplified implementation.
pub async fn count_children(_handle: &RuntimeHandle, _sup: Pid) -> Result<ChildCounts, String> {
    // This would need a proper call mechanism
    Err("count_children not yet implemented".to_string())
}

/// Terminates a child process.
///
/// Note: This is a simplified implementation.
pub async fn terminate_child(
    _handle: &RuntimeHandle,
    _sup: Pid,
    _id: &str,
) -> Result<(), TerminateError> {
    // This would need a proper call mechanism
    Err(TerminateError::NotFound("not implemented".to_string()))
}

/// Deletes a child specification.
///
/// Note: This is a simplified implementation.
pub async fn delete_child(
    _handle: &RuntimeHandle,
    _sup: Pid,
    _id: &str,
) -> Result<(), DeleteError> {
    // This would need a proper call mechanism
    Err(DeleteError::NotFound("not implemented".to_string()))
}
