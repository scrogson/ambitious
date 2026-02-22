//! Supervisor implementation.
//!
//! The supervisor manages child processes according to a supervision strategy.

use super::error::{DeleteError, RestartError, StartError, TerminateError};
use super::types::{
    ChildCounts, ChildInfo, ChildSpec, ChildType, RestartType, ShutdownType, Strategy,
    SupervisorFlags,
};
use crate::core::{ExitReason, Pid, Ref, SystemMessage, Term};
use crate::gen_server::via::{Name, register_name, unregister_name};
use crate::process::RuntimeHandle;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

// =============================================================================
// Supervisor Command Protocol
// =============================================================================

/// Tag used for supervisor command envelopes.
const SUPERVISOR_COMMAND_TAG: &str = "$supervisor_cmd";

/// Internal command messages sent to a supervisor process.
///
/// These commands are used by the public API functions (`terminate_child`,
/// `delete_child`, `restart_child`) to communicate with the supervisor loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum SupervisorCommand {
    /// Request to terminate a child process.
    Terminate {
        /// The child ID to terminate.
        id: String,
        /// PID to send the reply to.
        reply_to: Pid,
        /// Reference for matching the reply.
        reply_ref: Ref,
    },
    /// Request to delete a child specification (child must be stopped).
    Delete {
        /// The child ID to delete.
        id: String,
        /// PID to send the reply to.
        reply_to: Pid,
        /// Reference for matching the reply.
        reply_ref: Ref,
    },
    /// Request to restart a stopped child.
    Restart {
        /// The child ID to restart.
        id: String,
        /// PID to send the reply to.
        reply_to: Pid,
        /// Reference for matching the reply.
        reply_ref: Ref,
    },
}

/// Encodes a supervisor command into bytes using a tagged envelope.
fn encode_supervisor_command(cmd: &SupervisorCommand) -> Vec<u8> {
    let payload = Term::encode(cmd);
    let tagged = crate::gen_server::protocol::TaggedEnvelope::new(SUPERVISOR_COMMAND_TAG, payload);
    tagged.encode()
}

/// Tries to decode a supervisor command from bytes.
///
/// Returns `None` if the bytes are not a valid supervisor command envelope.
fn decode_supervisor_command(data: &[u8]) -> Option<SupervisorCommand> {
    let tagged = crate::gen_server::protocol::TaggedEnvelope::decode(data).ok()?;
    if tagged.tag != SUPERVISOR_COMMAND_TAG {
        return None;
    }
    Term::decode(&tagged.payload).ok()
}

/// Waits for a reply to a supervisor command.
///
/// Listens for a `Reply` protocol message with the matching reference.
/// Returns the raw payload bytes on success, or `Err(())` on timeout.
async fn wait_for_supervisor_reply(reference: Ref) -> Result<Vec<u8>, ()> {
    use crate::gen_server::protocol::Message as ProtocolMessage;

    let timeout = std::time::Duration::from_secs(5);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline - tokio::time::Instant::now();
        if remaining.is_zero() {
            return Err(());
        }

        let raw_msg = match crate::runtime::recv_timeout(remaining).await {
            Ok(Some(m)) => m,
            Ok(None) => return Err(()),
            Err(()) => return Err(()),
        };

        if let Ok(ProtocolMessage::Reply {
            reference: ref r,
            payload,
        }) = ProtocolMessage::decode(&raw_msg)
            && *r == reference
        {
            return Ok(payload);
        }

        tracing::trace!("supervisor: ignoring non-reply message while waiting for command reply");
    }
}

// =============================================================================
// Supervisor State Registry
// =============================================================================

/// Global registry of supervisor states for querying.
/// This allows `which_children` and `count_children` to access supervisor state.
static SUPERVISOR_STATES: OnceLock<DashMap<Pid, SupervisorQueryState>> = OnceLock::new();

fn get_supervisor_registry() -> &'static DashMap<Pid, SupervisorQueryState> {
    SUPERVISOR_STATES.get_or_init(DashMap::new)
}

/// Queryable state for a supervisor (subset of SupervisorState that can be shared).
struct SupervisorQueryState {
    /// Children indexed by ID.
    children: HashMap<String, QueryableChildState>,
}

/// Queryable state for a child.
struct QueryableChildState {
    /// The child's ID.
    id: String,
    /// The child's PID if running.
    pid: Option<Pid>,
    /// The child type (Worker or Supervisor).
    child_type: ChildType,
    /// The restart type.
    restart: RestartType,
}

impl SupervisorQueryState {
    fn get_child_counts(&self) -> ChildCounts {
        let mut counts = ChildCounts {
            specs: self.children.len(),
            active: 0,
            supervisors: 0,
            workers: 0,
        };

        for child in self.children.values() {
            if child.pid.is_some() {
                counts.active += 1;
            }
            match child.child_type {
                ChildType::Worker => counts.workers += 1,
                ChildType::Supervisor => counts.supervisors += 1,
            }
        }

        counts
    }

    fn get_which_children(&self) -> Vec<ChildInfo> {
        self.children
            .values()
            .map(|c| ChildInfo {
                id: c.id.clone(),
                pid: c.pid,
                child_type: c.child_type,
                restart: c.restart,
            })
            .collect()
    }
}

/// The Supervisor trait for implementing supervision trees.
///
/// Supervisors manage child processes and handle their failures according
/// to a configurable strategy.
///
/// # Example
///
/// ```ignore
/// use ambitious_supervisor::{Supervisor, SupervisorInit, SupervisorFlags, ChildSpec, Strategy};
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
    /// Optional registered name for this supervisor.
    name: Option<Name>,
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
            crate::runtime::with_ctx(|ctx| ctx.monitor(pid)).map_err(|e| format!("{}", e))?;

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
        if let Some(child) = self.children.get_mut(id)
            && let Some(pid) = child.pid.take()
        {
            self.pid_to_id.remove(&pid);

            // Demonitor first using task-local context
            if let Some(ref_) = child.monitor_ref.take() {
                crate::runtime::with_ctx(|ctx| ctx.demonitor(ref_));
            }

            // Terminate based on shutdown type
            let shutdown = child.spec.shutdown;
            match shutdown {
                ShutdownType::BrutalKill => {
                    // Immediate kill without waiting
                    let _ = crate::runtime::with_ctx(|ctx| ctx.exit(pid, ExitReason::Killed));
                }
                ShutdownType::Timeout(duration) => {
                    // Send shutdown signal
                    let _ = crate::runtime::with_ctx(|ctx| ctx.exit(pid, ExitReason::Shutdown));

                    // Wait for child to terminate with timeout
                    if !wait_for_termination(pid, Some(duration)).await {
                        // Timed out - force kill
                        let _ = crate::runtime::with_ctx(|ctx| ctx.exit(pid, ExitReason::Killed));
                        // Give it a moment to process the kill
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                ShutdownType::Infinity => {
                    // Send shutdown signal and wait indefinitely
                    let _ = crate::runtime::with_ctx(|ctx| ctx.exit(pid, ExitReason::Shutdown));
                    wait_for_termination(pid, None).await;
                }
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
                restart: c.spec.restart,
            })
            .collect()
    }

    /// Syncs the supervisor state to the global registry for queries.
    fn sync_to_registry(&self) {
        let query_state = SupervisorQueryState {
            children: self
                .children
                .iter()
                .map(|(id, child)| {
                    (
                        id.clone(),
                        QueryableChildState {
                            id: child.spec.id.clone(),
                            pid: child.pid,
                            child_type: child.spec.child_type,
                            restart: child.spec.restart,
                        },
                    )
                })
                .collect(),
        };
        get_supervisor_registry().insert(self.self_pid, query_state);
    }

    /// Removes this supervisor from the global registry.
    fn unregister_from_registry(&self) {
        get_supervisor_registry().remove(&self.self_pid);
    }

    /// Unregisters the supervisor's name if one was set.
    fn unregister_name(&self) {
        if let Some(ref name) = self.name {
            unregister_name(name);
        }
    }

    /// Handles a `TerminateChild` command.
    ///
    /// Terminates the child if it exists (running or not). Returns an error
    /// if the child ID is not found.
    async fn handle_terminate_child_command(&mut self, id: &str) -> Result<(), String> {
        if !self.children.contains_key(id) {
            return Err(format!("child '{}' not found", id));
        }
        self.terminate_child_by_id(id).await;
        Ok(())
    }

    /// Handles a `DeleteChild` command.
    ///
    /// Removes the child specification. The child must not be running.
    fn handle_delete_child_command(&mut self, id: &str) -> Result<(), String> {
        let child = self
            .children
            .get(id)
            .ok_or_else(|| format!("not_found:{}", id))?;
        if child.pid.is_some() {
            return Err(format!("running:{}", id));
        }
        self.children.remove(id);
        self.child_order.retain(|i| i != id);
        Ok(())
    }

    /// Handles a `RestartChild` command.
    ///
    /// Restarts a stopped child. The child must exist and not be running.
    async fn handle_restart_child_command(&mut self, id: &str) -> Result<Pid, String> {
        let child = self
            .children
            .get(id)
            .ok_or_else(|| format!("not_found:{}", id))?;
        if child.pid.is_some() {
            return Err(format!("already_running:{}", id));
        }
        self.start_child(id).await
    }
}

/// The main supervisor process loop.
async fn supervisor_loop(mut state: SupervisorState) {
    // Start all initial children
    if let Err(reason) = state.start_all_children().await {
        // Failed to start children - supervisor terminates with error
        // Set exit reason for failure escalation
        crate::runtime::set_exit_reason_async(reason).await;
        state.unregister_name();
        state.unregister_from_registry();
        return;
    }

    // Register name after children started successfully
    if let Some(ref name) = state.name
        && register_name(name, state.self_pid).is_err()
    {
        // Name already taken — tear down children and exit
        state.terminate_all_children().await;
        state.unregister_from_registry();
        return;
    }

    // Register initial state in the global registry
    state.sync_to_registry();

    // Main message loop
    loop {
        let msg = match crate::runtime::recv().await {
            Some(m) => m,
            None => {
                // Mailbox closed - terminate children and exit
                state.terminate_all_children().await;
                state.unregister_name();
                state.unregister_from_registry();
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
            if let Err(exit_reason) = state.handle_child_exit(pid, reason).await {
                // Supervisor needs to stop due to restart intensity
                // Set exit reason for failure escalation to parent supervisor
                crate::runtime::set_exit_reason_async(exit_reason).await;
                state.terminate_all_children().await;
                state.unregister_name();
                state.unregister_from_registry();
                return;
            }
            // Sync state after child exit/restart
            state.sync_to_registry();
        }

        // Check for exit signals
        if let Ok(SystemMessage::Exit { from: _, reason }) = <SystemMessage as Term>::decode(&msg) {
            // If we receive an exit signal, terminate with that reason
            crate::runtime::set_exit_reason_async(reason).await;
            state.terminate_all_children().await;
            state.unregister_name();
            state.unregister_from_registry();
            return;
        }

        // Check for supervisor commands
        if let Some(cmd) = decode_supervisor_command(&msg) {
            match cmd {
                SupervisorCommand::Terminate {
                    id,
                    reply_to,
                    reply_ref,
                } => {
                    let result = state.handle_terminate_child_command(&id).await;
                    state.sync_to_registry();
                    let reply_bytes = Term::encode(&result);
                    let reply_msg =
                        crate::gen_server::protocol::encode_reply(reply_ref, &reply_bytes);
                    let _ = crate::send_raw(reply_to, reply_msg);
                }
                SupervisorCommand::Delete {
                    id,
                    reply_to,
                    reply_ref,
                } => {
                    let result = state.handle_delete_child_command(&id);
                    state.sync_to_registry();
                    let reply_bytes = Term::encode(&result);
                    let reply_msg =
                        crate::gen_server::protocol::encode_reply(reply_ref, &reply_bytes);
                    let _ = crate::send_raw(reply_to, reply_msg);
                }
                SupervisorCommand::Restart {
                    id,
                    reply_to,
                    reply_ref,
                } => {
                    let result = state.handle_restart_child_command(&id).await;
                    state.sync_to_registry();
                    let reply_bytes = Term::encode(&result);
                    let reply_msg =
                        crate::gen_server::protocol::encode_reply(reply_ref, &reply_bytes);
                    let _ = crate::send_raw(reply_to, reply_msg);
                }
            }
        }
    }
}

/// Options for starting a Supervisor.
///
/// # Example
///
/// ```ignore
/// use ambitious::gen_server::Name;
/// use ambitious::supervisor::SupervisorStartOpts;
///
/// let pid = SupervisorStartOpts::new(&handle, ())
///     .name(Name::local("my_sup"))
///     .link(parent_pid)
///     .start::<MySupervisor>()
///     .await?;
/// ```
pub struct SupervisorStartOpts<'a, A> {
    handle: &'a RuntimeHandle,
    arg: A,
    name: Option<Name>,
    link: Option<Pid>,
}

impl<'a, A: Send + 'static> SupervisorStartOpts<'a, A> {
    /// Create start options with the given handle and init arg.
    pub fn new(handle: &'a RuntimeHandle, arg: A) -> Self {
        Self {
            handle,
            arg,
            name: None,
            link: None,
        }
    }

    /// Register the supervisor under the given name after children start.
    pub fn name(mut self, name: Name) -> Self {
        self.name = Some(name);
        self
    }

    /// Link the supervisor to the given parent process.
    pub fn link(mut self, parent: Pid) -> Self {
        self.link = Some(parent);
        self
    }

    /// Start the supervisor with these options.
    pub async fn start<S: Supervisor<InitArg = A>>(self) -> Result<Pid, StartError> {
        start_impl::<S>(self.handle, self.arg, self.name, self.link).await
    }
}

/// Starts a supervisor with the given implementation, linked to a parent.
///
/// Returns the PID of the started supervisor.
pub async fn start_link<S: Supervisor>(
    handle: &RuntimeHandle,
    parent: Pid,
    arg: S::InitArg,
) -> Result<Pid, StartError> {
    start_impl::<S>(handle, arg, None, Some(parent)).await
}

/// Starts a supervisor without linking.
pub async fn start<S: Supervisor>(
    handle: &RuntimeHandle,
    arg: S::InitArg,
) -> Result<Pid, StartError> {
    start_impl::<S>(handle, arg, None, None).await
}

async fn start_impl<S: Supervisor>(
    handle: &RuntimeHandle,
    arg: S::InitArg,
    name: Option<Name>,
    link: Option<Pid>,
) -> Result<Pid, StartError> {
    let init_result = S::init(arg);

    let handle_clone = handle.clone();

    let spawn_fn = move || {
        let self_pid = crate::runtime::current_pid();

        // Set up trap_exit so we get exit signals as messages
        crate::runtime::with_ctx(|ctx| ctx.set_trap_exit(true));

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
            name,
        };

        supervisor_loop(state)
    };

    let pid = if let Some(parent) = link {
        handle.spawn_link(parent, spawn_fn)
    } else {
        handle.spawn(spawn_fn)
    };

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
/// Returns a list of `ChildInfo` structs describing each child specification.
pub async fn which_children(_handle: &RuntimeHandle, sup: Pid) -> Result<Vec<ChildInfo>, String> {
    let registry = get_supervisor_registry();
    let state = registry
        .get(&sup)
        .ok_or_else(|| format!("supervisor {} not found", sup))?;
    Ok(state.get_which_children())
}

/// Gets counts of supervisor children.
///
/// Returns a `ChildCounts` struct with the number of specs, active children,
/// supervisors, and workers.
pub async fn count_children(_handle: &RuntimeHandle, sup: Pid) -> Result<ChildCounts, String> {
    let registry = get_supervisor_registry();
    let state = registry
        .get(&sup)
        .ok_or_else(|| format!("supervisor {} not found", sup))?;
    Ok(state.get_child_counts())
}

/// Terminates a child process managed by the given supervisor.
///
/// Sends a `TerminateChild` command to the supervisor and waits for the reply.
/// The child's process is stopped but its specification remains, allowing it
/// to be restarted later with [`restart_child`].
///
/// # Errors
///
/// Returns [`TerminateError::NotFound`] if the child ID does not exist, or
/// [`TerminateError::Timeout`] if the supervisor does not respond in time.
pub async fn terminate_child(
    _handle: &RuntimeHandle,
    sup: Pid,
    id: &str,
) -> Result<(), TerminateError> {
    let reply_ref = Ref::new();
    let caller_pid = crate::runtime::current_pid();

    let cmd = SupervisorCommand::Terminate {
        id: id.to_string(),
        reply_to: caller_pid,
        reply_ref,
    };

    crate::send_raw(sup, encode_supervisor_command(&cmd))
        .map_err(|_| TerminateError::NotFound(id.to_string()))?;

    let reply = wait_for_supervisor_reply(reply_ref)
        .await
        .map_err(|_| TerminateError::Timeout)?;

    let result: Result<(), String> =
        Term::decode(&reply).map_err(|_| TerminateError::NotFound(id.to_string()))?;

    result.map_err(TerminateError::NotFound)
}

/// Deletes a child specification from the supervisor.
///
/// Sends a `DeleteChild` command to the supervisor and waits for the reply.
/// The child must already be stopped (via [`terminate_child`]) before it can
/// be deleted.
///
/// # Errors
///
/// Returns [`DeleteError::Running`] if the child is still running, or
/// [`DeleteError::NotFound`] if the child ID does not exist.
pub async fn delete_child(_handle: &RuntimeHandle, sup: Pid, id: &str) -> Result<(), DeleteError> {
    let reply_ref = Ref::new();
    let caller_pid = crate::runtime::current_pid();

    let cmd = SupervisorCommand::Delete {
        id: id.to_string(),
        reply_to: caller_pid,
        reply_ref,
    };

    crate::send_raw(sup, encode_supervisor_command(&cmd))
        .map_err(|_| DeleteError::NotFound(id.to_string()))?;

    let reply = wait_for_supervisor_reply(reply_ref)
        .await
        .map_err(|_| DeleteError::NotFound(id.to_string()))?;

    let result: Result<(), String> =
        Term::decode(&reply).map_err(|_| DeleteError::NotFound(id.to_string()))?;

    result.map_err(|e| {
        if e.starts_with("running:") {
            DeleteError::Running(e.strip_prefix("running:").unwrap_or(&e).to_string())
        } else {
            DeleteError::NotFound(e.strip_prefix("not_found:").unwrap_or(&e).to_string())
        }
    })
}

/// Restarts a stopped child process.
///
/// Sends a `RestartChild` command to the supervisor and waits for the reply.
/// The child must be stopped (via [`terminate_child`] or having exited) before
/// it can be restarted.
///
/// # Errors
///
/// Returns [`RestartError::AlreadyRunning`] if the child is still running,
/// [`RestartError::NotFound`] if the child ID does not exist, or
/// [`RestartError::StartFailed`] if the child's start function fails.
pub async fn restart_child(
    _handle: &RuntimeHandle,
    sup: Pid,
    id: &str,
) -> Result<Pid, RestartError> {
    let reply_ref = Ref::new();
    let caller_pid = crate::runtime::current_pid();

    let cmd = SupervisorCommand::Restart {
        id: id.to_string(),
        reply_to: caller_pid,
        reply_ref,
    };

    crate::send_raw(sup, encode_supervisor_command(&cmd))
        .map_err(|_| RestartError::NotFound(id.to_string()))?;

    let reply = wait_for_supervisor_reply(reply_ref)
        .await
        .map_err(|_| RestartError::StartFailed("timeout waiting for reply".to_string()))?;

    let result: Result<Pid, String> =
        Term::decode(&reply).map_err(|_| RestartError::StartFailed("decode error".to_string()))?;

    result.map_err(|e| {
        if e.starts_with("already_running:") {
            RestartError::AlreadyRunning(
                e.strip_prefix("already_running:").unwrap_or(&e).to_string(),
            )
        } else if e.starts_with("not_found:") {
            RestartError::NotFound(e.strip_prefix("not_found:").unwrap_or(&e).to_string())
        } else {
            RestartError::StartFailed(e)
        }
    })
}

/// Waits for a process to terminate.
///
/// Returns `true` if the process terminated, `false` if the timeout expired.
/// If `timeout` is `None`, waits indefinitely.
///
/// Uses the task-local context's registry to check process liveness, so this
/// must be called from within a process context.
async fn wait_for_termination(pid: Pid, timeout: Option<Duration>) -> bool {
    const POLL_INTERVAL: Duration = Duration::from_millis(10);

    let deadline = timeout.map(|t| Instant::now() + t);

    loop {
        // Check if process is dead using the task-local context's registry
        let is_alive = crate::runtime::with_ctx(|ctx| ctx.is_alive(pid));
        if !is_alive {
            return true;
        }

        // Check timeout
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return false;
        }

        // Wait a bit before checking again
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
