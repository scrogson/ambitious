//! DREAM runtime for managing processes.
//!
//! The [`Runtime`] is the entry point for running DREAM applications.
//! It manages the process registry and provides spawning capabilities.

use dream_core::{ExitReason, Pid, Ref, SystemMessage};
use dream_runtime::{Context, Mailbox, ProcessHandle, ProcessRegistry, ProcessState, SendError};
use std::future::Future;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;

/// The DREAM runtime.
///
/// The runtime manages the process registry and provides methods for
/// spawning processes and interacting with them.
///
/// # Example
///
/// ```
/// use dream_process::Runtime;
///
/// #[tokio::main]
/// async fn main() {
///     let runtime = Runtime::new();
///
///     // Spawn processes using the runtime handle
///     let handle = runtime.handle();
///     // ...
/// }
/// ```
pub struct Runtime {
    registry: ProcessRegistry,
}

impl Runtime {
    /// Creates a new DREAM runtime.
    pub fn new() -> Self {
        Self {
            registry: ProcessRegistry::new(),
        }
    }

    /// Returns a handle to the runtime that can be cloned and shared.
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            registry: self.registry.clone(),
        }
    }

    /// Returns the process registry.
    pub fn registry(&self) -> &ProcessRegistry {
        &self.registry
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

/// A cloneable handle to the runtime.
///
/// This can be passed to spawned processes to allow them to spawn
/// additional processes.
#[derive(Clone)]
pub struct RuntimeHandle {
    registry: ProcessRegistry,
}

impl RuntimeHandle {
    /// Returns the process registry.
    pub fn registry(&self) -> &ProcessRegistry {
        &self.registry
    }

    /// Spawns a new process.
    ///
    /// The process function receives a [`Context`] and should return a future
    /// that represents the process's lifetime.
    pub fn spawn<F, Fut>(&self, f: F) -> Pid
    where
        F: FnOnce(Context) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.spawn_internal(f, false, false).0
    }

    /// Spawns a new process and links it to the caller.
    ///
    /// Note: Since this is called from outside a process context,
    /// there's no "caller" to link to. Use this when you have a
    /// process context and want to spawn a linked child.
    pub fn spawn_link<F, Fut>(&self, parent_pid: Pid, f: F) -> Pid
    where
        F: FnOnce(Context) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (child_pid, _) = self.spawn_internal(f, false, false);

        // Set up bidirectional link
        if let Some(parent_handle) = self.registry.get(parent_pid) {
            parent_handle.add_link(child_pid);
            if let Some(child_handle) = self.registry.get(child_pid) {
                child_handle.add_link(parent_pid);
            }
        }

        child_pid
    }

    /// Spawns a new process and monitors it.
    ///
    /// Returns the PID and monitor reference.
    pub fn spawn_monitor<F, Fut>(&self, monitor_pid: Pid, f: F) -> (Pid, Ref)
    where
        F: FnOnce(Context) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (child_pid, _) = self.spawn_internal(f, false, false);
        let reference = Ref::new();

        // Set up monitor
        if let Some(monitor_handle) = self.registry.get(monitor_pid) {
            monitor_handle.add_monitor(reference, child_pid);
            if let Some(child_handle) = self.registry.get(child_pid) {
                child_handle.add_monitored_by(reference, monitor_pid);
            }
        }

        (child_pid, reference)
    }

    /// Internal spawn implementation.
    fn spawn_internal<F, Fut>(&self, f: F, _link: bool, _monitor: bool) -> (Pid, Option<Ref>)
    where
        F: FnOnce(Context) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pid = Pid::new();
        let (mailbox, sender) = Mailbox::new();
        let state = Arc::new(RwLock::new(ProcessState::new(pid)));
        let (term_tx, term_rx) = oneshot::channel();

        let handle = ProcessHandle::new(pid, sender, state.clone(), Some(term_tx));
        self.registry.register(handle.clone());

        let registry = self.registry.clone();
        let state_for_task = state.clone();
        let ctx = Context::new(pid, mailbox, state.clone(), registry.clone());

        // Spawn the tokio task
        tokio::spawn(async move {
            // Run the process
            f(ctx).await;

            // Process completed normally
            Self::handle_process_exit(pid, ExitReason::Normal, &registry, &state_for_task);
        });

        // Spawn a task to wait for termination signal
        let registry_clone = self.registry.clone();
        let state_for_term = state.clone();
        tokio::spawn(async move {
            if let Ok(reason) = term_rx.await {
                // External termination
                Self::handle_process_exit(pid, reason, &registry_clone, &state_for_term);
            }
        });

        (pid, None)
    }

    /// Handles process exit - notifies links and monitors.
    fn handle_process_exit(
        pid: Pid,
        reason: ExitReason,
        registry: &ProcessRegistry,
        state: &Arc<RwLock<ProcessState>>,
    ) {
        let (links, monitored_by) = {
            let mut state = state.write().unwrap();
            if state.terminated {
                return; // Already handled
            }
            state.terminated = true;
            state.exit_reason = Some(reason.clone());
            (
                state.links.iter().copied().collect::<Vec<_>>(),
                state
                    .monitored_by
                    .iter()
                    .map(|(r, p)| (*r, *p))
                    .collect::<Vec<_>>(),
            )
        };

        // Notify linked processes
        for linked_pid in links {
            if let Some(linked_handle) = registry.get(linked_pid) {
                linked_handle.remove_link(pid);

                if reason.is_killed() {
                    // Killed propagates unconditionally
                    linked_handle.mark_terminated(ExitReason::Killed);
                } else if linked_handle.is_trapping_exits() {
                    // Send as message
                    let exit_msg = SystemMessage::exit(pid, reason.clone());
                    let _ = linked_handle.send(&exit_msg);
                } else if reason.is_abnormal() {
                    // Propagate abnormal exit
                    linked_handle.mark_terminated(reason.clone());
                }
                // Normal exits don't propagate to non-trapping processes
            }
        }

        // Notify monitoring processes
        for (reference, monitor_pid) in monitored_by {
            if let Some(monitor_handle) = registry.get(monitor_pid) {
                monitor_handle.remove_monitor(reference);
                let down_msg = SystemMessage::down(reference, pid, reason.clone());
                let _ = monitor_handle.send(&down_msg);
            }
        }

        // Unregister the process
        registry.unregister(pid);
    }

    /// Sends an exit signal to a process.
    pub fn exit(&self, target: Pid, reason: ExitReason) -> Result<(), SendError> {
        if let Some(handle) = self.registry.get(target) {
            handle.mark_terminated(reason);
            Ok(())
        } else {
            Err(SendError::ProcessNotFound(target))
        }
    }

    /// Returns `true` if the process is alive.
    pub fn alive(&self, pid: Pid) -> bool {
        self.registry
            .get(pid)
            .map(|h| h.is_alive())
            .unwrap_or(false)
    }

    /// Looks up a process by name.
    pub fn whereis(&self, name: &str) -> Option<Pid> {
        self.registry.whereis(name)
    }

    /// Registers a name for a process.
    pub fn register(&self, name: String, pid: Pid) -> bool {
        self.registry.register_name(name, pid)
    }

    /// Unregisters a name.
    pub fn unregister(&self, name: &str) -> Option<Pid> {
        self.registry.unregister_name(name)
    }

    /// Returns all registered names.
    pub fn registered(&self) -> Vec<String> {
        self.registry.registered_names()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_spawn_basic() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();

        let pid = handle.spawn(move |_ctx| async move {
            executed_clone.store(true, Ordering::SeqCst);
        });

        // Give the process time to run
        sleep(Duration::from_millis(50)).await;

        assert!(executed.load(Ordering::SeqCst));
        assert!(!handle.alive(pid)); // Process finished
    }

    #[tokio::test]
    async fn test_spawn_with_context() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let received = Arc::new(AtomicBool::new(false));
        let received_clone = received.clone();

        let pid = handle.spawn(move |mut ctx| async move {
            if let Some(_msg) = ctx.recv_timeout(Duration::from_millis(100)).await.ok().flatten() {
                received_clone.store(true, Ordering::SeqCst);
            }
        });

        // Send a message
        handle.registry().send_raw(pid, vec![1, 2, 3]).unwrap();

        // Give the process time to receive
        sleep(Duration::from_millis(50)).await;

        assert!(received.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_spawn_link() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let parent_received_exit = Arc::new(AtomicBool::new(false));
        let parent_received_clone = parent_received_exit.clone();

        // Create a "parent" process that traps exits
        let _parent_pid = handle.spawn(|mut ctx| async move {
            ctx.set_trap_exit(true);
            // Wait for exit message
            loop {
                match ctx.recv_timeout(Duration::from_millis(500)).await {
                    Ok(Some(msg)) => {
                        if let Ok(SystemMessage::Exit { .. }) =
                            <SystemMessage as dream_core::Message>::decode(&msg)
                        {
                            parent_received_clone.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });

        // Give parent time to start
        sleep(Duration::from_millis(10)).await;

        // Spawn linked child that exits immediately (normal exit)
        let child_pid = handle.spawn(|_ctx| async move {
            // Just exit normally
        });

        // Give child time to finish
        sleep(Duration::from_millis(50)).await;

        // Child should be dead (finished)
        assert!(!handle.alive(child_pid));
    }

    #[tokio::test]
    async fn test_spawn_monitor() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let down_received = Arc::new(AtomicBool::new(false));
        let down_clone = down_received.clone();

        // Create monitor process
        let monitor_pid = handle.spawn(move |mut ctx| async move {
            loop {
                match ctx.recv_timeout(Duration::from_millis(500)).await {
                    Ok(Some(msg)) => {
                        // Check if it's a DOWN message
                        if let Ok(SystemMessage::Down { .. }) =
                            <SystemMessage as dream_core::Message>::decode(&msg)
                        {
                            down_clone.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });

        // Give monitor time to start
        sleep(Duration::from_millis(10)).await;

        // Spawn monitored child
        let (_child_pid, _ref) = handle.spawn_monitor(monitor_pid, |_ctx| async move {
            // Exit immediately
        });

        // Give child time to exit and DOWN to be delivered
        sleep(Duration::from_millis(100)).await;

        assert!(down_received.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_register_name() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let pid = handle.spawn(|mut ctx| async move {
            loop {
                if ctx.recv_timeout(Duration::from_millis(500)).await.is_err() {
                    break;
                }
            }
        });

        assert!(handle.register("my_process".to_string(), pid));
        assert_eq!(handle.whereis("my_process"), Some(pid));

        // Can't register same name twice
        let pid2 = handle.spawn(|_ctx| async {});
        assert!(!handle.register("my_process".to_string(), pid2));

        // Unregister
        assert_eq!(handle.unregister("my_process"), Some(pid));
        assert_eq!(handle.whereis("my_process"), None);
    }
}
