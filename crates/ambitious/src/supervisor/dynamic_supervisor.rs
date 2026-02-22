//! DynamicSupervisor implementation.
//!
//! A DynamicSupervisor starts with no children and children are started
//! dynamically via `start_child`. Unlike regular supervisors, there is
//! no ordering between children, and children are identified by PID rather
//! than by ID.
//!
//! DynamicSupervisor only supports the one-for-one strategy.
//!
//! # Example
//!
//! ```ignore
//! use ambitious_supervisor::dynamic_supervisor::{self, DynamicSupervisorOpts};
//! use ambitious_supervisor::ChildSpec;
//!
//! // Start a dynamic supervisor
//! let sup_pid = dynamic_supervisor::start_link(DynamicSupervisorOpts::new()).await?;
//!
//! // Start children dynamically
//! let spec = ChildSpec::new("worker", || async {
//!     let pid = ambitious::spawn(|| async { /* worker code */ });
//!     Ok(pid)
//! });
//!
//! let child_pid = dynamic_supervisor::start_child(sup_pid, spec).await?;
//!
//! // Terminate a child
//! dynamic_supervisor::terminate_child(sup_pid, child_pid).await?;
//!
//! // Get counts
//! let counts = dynamic_supervisor::count_children(sup_pid).await?;
//! ```

use super::error::StartError;
use super::types::{ChildCounts, ChildInfo, ChildSpec, ChildType, RestartType, StartChildError};
use crate::core::{ExitReason, Pid, Ref, SystemMessage, Term};
use crate::gen_server::via::{Name, register_name, unregister_name};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Global registry of dynamic supervisors.
static SUPERVISORS: once_cell::sync::Lazy<DashMap<Pid, Arc<SupervisorState>>> =
    once_cell::sync::Lazy::new(DashMap::new);

/// Options for configuring a DynamicSupervisor.
#[derive(Clone)]
pub struct DynamicSupervisorOpts {
    /// Maximum number of restarts allowed in the time period.
    pub max_restarts: u32,
    /// Time period in seconds for restart counting.
    pub max_seconds: u32,
    /// Optional registered name for this supervisor.
    pub name: Option<Name>,
}

impl Default for DynamicSupervisorOpts {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            max_seconds: 5,
            name: None,
        }
    }
}

impl std::fmt::Debug for DynamicSupervisorOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicSupervisorOpts")
            .field("max_restarts", &self.max_restarts)
            .field("max_seconds", &self.max_seconds)
            .field("name", &self.name)
            .finish()
    }
}

impl DynamicSupervisorOpts {
    /// Creates new options with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum restarts.
    pub fn max_restarts(mut self, max: u32) -> Self {
        self.max_restarts = max;
        self
    }

    /// Sets the time period for restart counting.
    pub fn max_seconds(mut self, secs: u32) -> Self {
        self.max_seconds = secs;
        self
    }

    /// Register the supervisor under the given name after start.
    pub fn name(mut self, name: Name) -> Self {
        self.name = Some(name);
        self
    }
}

/// Internal state of a running child.
struct ChildState {
    /// The child specification.
    spec: ChildSpec,
    /// Monitor reference.
    monitor_ref: Ref,
}

/// Shared supervisor state.
struct SupervisorState {
    /// Options.
    opts: DynamicSupervisorOpts,
    /// Children indexed by PID.
    children: DashMap<Pid, ChildState>,
    /// Monitor ref to PID mapping.
    monitor_to_pid: DashMap<Ref, Pid>,
    /// Restart history for rate limiting.
    restart_times: Mutex<Vec<Instant>>,
    /// Whether the supervisor has been shut down.
    shutdown: Mutex<bool>,
    /// Optional registered name.
    name: Option<Name>,
}

impl SupervisorState {
    fn new(opts: DynamicSupervisorOpts) -> Self {
        let name = opts.name.clone();
        Self {
            opts,
            children: DashMap::new(),
            monitor_to_pid: DashMap::new(),
            restart_times: Mutex::new(Vec::new()),
            shutdown: Mutex::new(false),
            name,
        }
    }

    /// Starts a child and monitors it.
    async fn do_start_child(&self, spec: ChildSpec) -> Result<Pid, StartChildError> {
        if *self.shutdown.lock() {
            return Err(StartChildError::Failed(
                "supervisor is shutting down".to_string(),
            ));
        }

        // Call the start function
        let pid = (spec.start)().await?;

        // Monitor the child
        let monitor_ref = crate::runtime::with_ctx(|ctx| ctx.monitor(pid))
            .map_err(|e| StartChildError::Failed(e.to_string()))?;

        // Store the child
        self.children.insert(pid, ChildState { spec, monitor_ref });
        self.monitor_to_pid.insert(monitor_ref, pid);

        Ok(pid)
    }

    /// Handles a child exit.
    async fn handle_child_exit(&self, pid: Pid, reason: ExitReason) -> Result<(), ExitReason> {
        let child_state = match self.children.remove(&pid) {
            Some((_, state)) => state,
            None => return Ok(()), // Not our child
        };

        // Remove monitor mapping
        self.monitor_to_pid.remove(&child_state.monitor_ref);

        let should_restart = match child_state.spec.restart {
            RestartType::Permanent => true,
            RestartType::Transient => reason.is_abnormal(),
            RestartType::Temporary => false,
        };

        if !should_restart {
            return Ok(());
        }

        // Check restart rate
        let now = Instant::now();
        {
            let mut restart_times = self.restart_times.lock();
            let cutoff = now - Duration::from_secs(self.opts.max_seconds as u64);
            restart_times.retain(|t| *t > cutoff);

            if restart_times.len() >= self.opts.max_restarts as usize {
                return Err(ExitReason::error("max restart intensity reached"));
            }

            restart_times.push(now);
        }

        // Restart the child
        match self.do_start_child(child_state.spec).await {
            Ok(_) => Ok(()),
            Err(StartChildError::Ignore) => Ok(()),
            Err(e) => Err(ExitReason::error(format!("child failed to restart: {}", e))),
        }
    }

    /// Terminates a specific child.
    fn do_terminate_child(&self, pid: Pid) -> Result<(), String> {
        if let Some((_, child)) = self.children.remove(&pid) {
            self.monitor_to_pid.remove(&child.monitor_ref);
            let _ = crate::runtime::with_ctx(|ctx| {
                ctx.demonitor(child.monitor_ref);
                ctx.exit(pid, ExitReason::Shutdown)
            });
            Ok(())
        } else {
            Err("child not found".to_string())
        }
    }

    /// Terminates all children.
    fn terminate_all_children(&self) {
        *self.shutdown.lock() = true;

        let pids: Vec<Pid> = self.children.iter().map(|r| *r.key()).collect();

        for pid in pids {
            if let Some((_, child)) = self.children.remove(&pid) {
                self.monitor_to_pid.remove(&child.monitor_ref);
                let _ = crate::runtime::with_ctx(|ctx| {
                    ctx.demonitor(child.monitor_ref);
                    ctx.exit(pid, ExitReason::Shutdown)
                });
            }
        }
    }

    /// Gets counts of children.
    fn get_child_counts(&self) -> ChildCounts {
        let mut supervisors = 0;
        let mut workers = 0;

        for entry in self.children.iter() {
            match entry.value().spec.child_type {
                ChildType::Supervisor => supervisors += 1,
                ChildType::Worker => workers += 1,
            }
        }

        let active = self.children.len();
        ChildCounts {
            specs: active,
            active,
            supervisors,
            workers,
        }
    }

    /// Gets information about all children.
    fn get_which_children(&self) -> Vec<ChildInfo> {
        self.children
            .iter()
            .enumerate()
            .map(|(i, entry)| ChildInfo {
                id: format!("child_{}", i),
                pid: Some(*entry.key()),
                child_type: entry.value().spec.child_type,
                restart: entry.value().spec.restart,
            })
            .collect()
    }
}

/// The main dynamic supervisor process loop.
async fn dynamic_supervisor_loop(state: Arc<SupervisorState>) {
    // Register name if configured
    if let Some(ref name) = state.name {
        let pid = crate::runtime::current_pid();
        if register_name(name, pid).is_err() {
            // Name already taken — exit immediately
            return;
        }
    }

    loop {
        let msg = match crate::runtime::recv().await {
            Some(m) => m,
            None => {
                state.terminate_all_children();
                if let Some(ref name) = state.name {
                    unregister_name(name);
                }
                return;
            }
        };

        // Check for DOWN messages from monitored children
        if let Ok(SystemMessage::Down {
            monitor_ref,
            pid,
            reason,
        }) = <SystemMessage as Term>::decode(&msg)
        {
            if state.monitor_to_pid.contains_key(&monitor_ref)
                && let Err(_exit_reason) = state.handle_child_exit(pid, reason).await
            {
                state.terminate_all_children();
                if let Some(ref name) = state.name {
                    unregister_name(name);
                }
                return;
            }
            continue;
        }

        // Check for exit signals
        if let Ok(SystemMessage::Exit { from: _, reason: _ }) =
            <SystemMessage as Term>::decode(&msg)
        {
            state.terminate_all_children();
            if let Some(ref name) = state.name {
                unregister_name(name);
            }
            return;
        }
    }
}

// === Public API ===

/// Starts a DynamicSupervisor linked to the calling process.
///
/// Returns the PID of the supervisor.
///
/// # Example
///
/// ```ignore
/// let sup_pid = dynamic_supervisor::start_link(DynamicSupervisorOpts::new()).await?;
/// ```
pub async fn start_link(opts: DynamicSupervisorOpts) -> Result<Pid, StartError> {
    let parent = crate::runtime::current_pid();
    let state = Arc::new(SupervisorState::new(opts));
    let state_clone = state.clone();

    let pid = crate::process::global::spawn_link(parent, move || {
        crate::runtime::with_ctx(|ctx| ctx.set_trap_exit(true));
        dynamic_supervisor_loop(state_clone)
    });

    // Register the supervisor state
    SUPERVISORS.insert(pid, state);

    // Give it a moment to initialize
    tokio::time::sleep(Duration::from_millis(10)).await;

    Ok(pid)
}

/// Starts a DynamicSupervisor without linking.
///
/// Returns the PID of the supervisor.
pub async fn start(opts: DynamicSupervisorOpts) -> Result<Pid, StartError> {
    let state = Arc::new(SupervisorState::new(opts));
    let state_clone = state.clone();

    let pid = crate::process::global::spawn(move || {
        crate::runtime::with_ctx(|ctx| ctx.set_trap_exit(true));
        dynamic_supervisor_loop(state_clone)
    });

    // Register the supervisor state
    SUPERVISORS.insert(pid, state);

    // Give it a moment to initialize
    tokio::time::sleep(Duration::from_millis(10)).await;

    Ok(pid)
}

/// Starts a child under the given dynamic supervisor.
///
/// # Arguments
///
/// * `supervisor` - The PID of the dynamic supervisor
/// * `spec` - The child specification
///
/// # Returns
///
/// The PID of the started child.
///
/// # Example
///
/// ```ignore
/// let child_pid = dynamic_supervisor::start_child(sup_pid, child_spec).await?;
/// ```
pub async fn start_child(supervisor: Pid, spec: ChildSpec) -> Result<Pid, StartChildError> {
    let state = SUPERVISORS
        .get(&supervisor)
        .ok_or_else(|| StartChildError::Failed("supervisor not found".to_string()))?;

    state.do_start_child(spec).await
}

/// Terminates a child of the given dynamic supervisor.
///
/// # Arguments
///
/// * `supervisor` - The PID of the dynamic supervisor
/// * `child` - The PID of the child to terminate
///
/// # Example
///
/// ```ignore
/// dynamic_supervisor::terminate_child(sup_pid, child_pid)?;
/// ```
pub fn terminate_child(supervisor: Pid, child: Pid) -> Result<(), String> {
    let state = SUPERVISORS
        .get(&supervisor)
        .ok_or_else(|| "supervisor not found".to_string())?;

    state.do_terminate_child(child)
}

/// Returns a map containing count values for the given supervisor.
///
/// # Arguments
///
/// * `supervisor` - The PID of the dynamic supervisor
///
/// # Example
///
/// ```ignore
/// let counts = dynamic_supervisor::count_children(sup_pid)?;
/// println!("Active: {}", counts.active);
/// ```
pub fn count_children(supervisor: Pid) -> Result<ChildCounts, String> {
    let state = SUPERVISORS
        .get(&supervisor)
        .ok_or_else(|| "supervisor not found".to_string())?;

    Ok(state.get_child_counts())
}

/// Returns a list with information about all children.
///
/// # Arguments
///
/// * `supervisor` - The PID of the dynamic supervisor
pub fn which_children(supervisor: Pid) -> Result<Vec<ChildInfo>, String> {
    let state = SUPERVISORS
        .get(&supervisor)
        .ok_or_else(|| "supervisor not found".to_string())?;

    Ok(state.get_which_children())
}

/// Stops the dynamic supervisor.
///
/// # Arguments
///
/// * `supervisor` - The PID of the dynamic supervisor
/// * `reason` - The exit reason
pub fn stop(supervisor: Pid, reason: ExitReason) -> Result<(), String> {
    // Remove from registry
    if let Some((_, state)) = SUPERVISORS.remove(&supervisor) {
        state.terminate_all_children();
    }

    // Send exit to the supervisor process
    crate::runtime::with_ctx(|ctx| ctx.exit(supervisor, reason)).map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_start_dynamic_supervisor() {
        crate::process::global::init();
        let handle = crate::process::global::handle();

        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();

        handle.spawn(move || async move {
            let sup_pid = start(DynamicSupervisorOpts::new()).await.unwrap();

            // Should have no children initially
            let counts = count_children(sup_pid).unwrap();
            if counts.active == 0 && counts.specs == 0 {
                test_passed_clone.store(true, Ordering::SeqCst);
            }
        });

        sleep(Duration::from_millis(100)).await;
        assert!(test_passed.load(Ordering::SeqCst), "test failed");
    }

    #[tokio::test]
    async fn test_start_child_api() {
        crate::process::global::init();
        let handle = crate::process::global::handle();

        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();

        handle.spawn(move || async move {
            let sup_pid = start(DynamicSupervisorOpts::new()).await.unwrap();

            let spec = ChildSpec::new("worker", || async {
                let pid = crate::process::global::spawn(|| async {
                    while let Ok(Some(_)) =
                        crate::runtime::recv_timeout(Duration::from_secs(60)).await
                    {
                    }
                });
                Ok(pid)
            });

            if let Ok(_child_pid) = start_child(sup_pid, spec).await {
                let counts = count_children(sup_pid).unwrap();
                if counts.active == 1 && counts.workers == 1 {
                    test_passed_clone.store(true, Ordering::SeqCst);
                }
            }
        });

        sleep(Duration::from_millis(100)).await;
        assert!(
            test_passed.load(Ordering::SeqCst),
            "start_child test failed"
        );
    }

    #[tokio::test]
    async fn test_terminate_child_api() {
        crate::process::global::init();
        let handle = crate::process::global::handle();

        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();

        handle.spawn(move || async move {
            let sup_pid = start(DynamicSupervisorOpts::new()).await.unwrap();

            let spec = ChildSpec::new("worker", || async {
                let pid = crate::process::global::spawn(|| async {
                    while let Ok(Some(_)) =
                        crate::runtime::recv_timeout(Duration::from_secs(60)).await
                    {
                    }
                });
                Ok(pid)
            });

            if let Ok(child_pid) = start_child(sup_pid, spec).await {
                // Terminate the child
                let _ = terminate_child(sup_pid, child_pid);

                sleep(Duration::from_millis(50)).await;

                let counts = count_children(sup_pid).unwrap();
                if counts.active == 0 {
                    test_passed_clone.store(true, Ordering::SeqCst);
                }
            }
        });

        sleep(Duration::from_millis(200)).await;
        assert!(
            test_passed.load(Ordering::SeqCst),
            "terminate_child test failed"
        );
    }

    #[tokio::test]
    async fn test_multiple_children_api() {
        crate::process::global::init();
        let handle = crate::process::global::handle();

        let child_count = Arc::new(AtomicUsize::new(0));
        let child_count_clone = child_count.clone();
        let final_count = Arc::new(AtomicUsize::new(0));
        let final_count_clone = final_count.clone();

        handle.spawn(move || async move {
            let sup_pid = start(DynamicSupervisorOpts::new()).await.unwrap();

            // Start multiple children
            for i in 0..5 {
                let spec = ChildSpec::new(format!("worker_{}", i), || async {
                    let pid = crate::process::global::spawn(|| async {
                        while let Ok(Some(_)) =
                            crate::runtime::recv_timeout(Duration::from_secs(60)).await
                        {
                        }
                    });
                    Ok(pid)
                });

                if start_child(sup_pid, spec).await.is_ok() {
                    child_count_clone.fetch_add(1, Ordering::SeqCst);
                }
            }

            let counts = count_children(sup_pid).unwrap();
            final_count_clone.store(counts.active, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(200)).await;

        assert_eq!(
            child_count.load(Ordering::SeqCst),
            5,
            "Should have started 5 children"
        );
        assert_eq!(
            final_count.load(Ordering::SeqCst),
            5,
            "Should have 5 active children"
        );
    }
}
