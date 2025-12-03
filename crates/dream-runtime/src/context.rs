//! Process execution context.
//!
//! The [`Context`] provides a process with access to runtime services
//! like sending messages, creating links/monitors, and spawning new processes.

use crate::mailbox::Mailbox;
use crate::process_handle::{ProcessHandle, ProcessState};
use crate::registry::ProcessRegistry;
use crate::SendError;
use dream_core::{ExitReason, Pid, Ref, SystemMessage, Term};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// The execution context for a process.
///
/// A `Context` is passed to each process and provides access to:
/// - The process's own PID
/// - The process's mailbox for receiving messages
/// - Methods for sending messages to other processes
/// - Methods for creating links and monitors
///
/// # Examples
///
/// ```ignore
/// async fn my_process(ctx: Context) {
///     let my_pid = ctx.pid();
///
///     // Receive a message
///     if let Some(envelope) = ctx.recv().await {
///         // Handle message
///     }
///
///     // Send a message to another process
///     ctx.send(other_pid, &MyMessage { data: 42 }).unwrap();
/// }
/// ```
pub struct Context {
    /// Our process ID.
    pid: Pid,
    /// Our mailbox for receiving messages.
    mailbox: Mailbox,
    /// Our process state.
    state: Arc<RwLock<ProcessState>>,
    /// Reference to the process registry.
    registry: ProcessRegistry,
}

impl Context {
    /// Creates a new context for a process.
    pub fn new(
        pid: Pid,
        mailbox: Mailbox,
        state: Arc<RwLock<ProcessState>>,
        registry: ProcessRegistry,
    ) -> Self {
        Self {
            pid,
            mailbox,
            state,
            registry,
        }
    }

    /// Returns this process's PID.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Receives the next message from the mailbox.
    ///
    /// Returns `None` if the mailbox is closed.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.mailbox.recv().await.map(|e| e.data)
    }

    /// Receives the next message with a timeout.
    ///
    /// Returns `Ok(Some(data))` if a message was received,
    /// `Ok(None)` if the mailbox was closed,
    /// or `Err(())` if the timeout elapsed.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, ()> {
        self.mailbox
            .recv_timeout(timeout)
            .await
            .map(|opt| opt.map(|e| e.data))
    }

    /// Tries to receive a message without blocking.
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        self.mailbox.try_recv().ok().map(|e| e.data)
    }

    /// Sends a raw message to another process.
    pub fn send_raw(&self, pid: Pid, data: Vec<u8>) -> Result<(), SendError> {
        self.registry.send_raw(pid, data)
    }

    /// Sends a typed message to another process.
    pub fn send<M: Term>(&self, pid: Pid, msg: &M) -> Result<(), SendError> {
        self.registry.send(pid, msg)
    }

    /// Sets the trap_exit flag for this process.
    ///
    /// When `true`, exit signals from linked processes are delivered
    /// as `SystemMessage::Exit` messages instead of terminating this process.
    ///
    /// Returns the previous value.
    pub fn set_trap_exit(&self, trap: bool) -> bool {
        let mut state = self.state.write().unwrap();
        let prev = state.trap_exit;
        state.trap_exit = trap;
        prev
    }

    /// Returns whether this process is trapping exits.
    pub fn is_trapping_exits(&self) -> bool {
        let state = self.state.read().unwrap();
        state.trap_exit
    }

    /// Creates a bidirectional link with another process.
    ///
    /// If either process terminates abnormally, the other will receive
    /// an exit signal.
    pub fn link(&self, other: Pid) -> Result<(), SendError> {
        // Get our handle to update our links
        {
            let mut state = self.state.write().unwrap();
            state.links.insert(other);
        }

        // Get the other process's handle to update their links
        if let Some(other_handle) = self.registry.get(other) {
            other_handle.add_link(self.pid);
            Ok(())
        } else {
            // Other process doesn't exist - remove our link and error
            let mut state = self.state.write().unwrap();
            state.links.remove(&other);
            Err(SendError::ProcessNotFound(other))
        }
    }

    /// Removes a link with another process.
    pub fn unlink(&self, other: Pid) {
        // Remove from our links
        {
            let mut state = self.state.write().unwrap();
            state.links.remove(&other);
        }

        // Remove from their links
        if let Some(other_handle) = self.registry.get(other) {
            other_handle.remove_link(self.pid);
        }
    }

    /// Creates a monitor on another process.
    ///
    /// Returns a reference that will be included in the `DOWN` message
    /// when the monitored process terminates.
    pub fn monitor(&self, target: Pid) -> Result<Ref, SendError> {
        let reference = Ref::new();

        // Record that we're monitoring the target
        {
            let mut state = self.state.write().unwrap();
            state.monitors.insert(reference, target);
        }

        // Tell the target they're being monitored
        if let Some(target_handle) = self.registry.get(target) {
            target_handle.add_monitored_by(reference, self.pid);
            Ok(reference)
        } else {
            // Target doesn't exist - send immediate DOWN message
            let mut state = self.state.write().unwrap();
            state.monitors.remove(&reference);

            // Queue a DOWN message for ourselves
            let down = SystemMessage::down(reference, target, ExitReason::error("noproc"));
            let _ = self.registry.send(self.pid, &down);

            Ok(reference)
        }
    }

    /// Removes a monitor.
    ///
    /// The reference will no longer be valid and no `DOWN` message
    /// will be sent for this monitor.
    pub fn demonitor(&self, reference: Ref) {
        // Get the target PID and remove from our monitors
        let target = {
            let mut state = self.state.write().unwrap();
            state.monitors.remove(&reference)
        };

        // Remove from target's monitored_by
        if let Some(target_pid) = target {
            if let Some(target_handle) = self.registry.get(target_pid) {
                target_handle.remove_monitored_by(reference);
            }
        }
    }

    /// Sends an exit signal to another process.
    ///
    /// If `reason` is `ExitReason::Killed`, the target will terminate
    /// unconditionally. Otherwise, the behavior depends on whether
    /// the target is trapping exits.
    pub fn exit(&self, target: Pid, reason: ExitReason) -> Result<(), SendError> {
        if let Some(handle) = self.registry.get(target) {
            if reason.is_killed() {
                // Killed is unconditional - mark terminated directly
                handle.mark_terminated(reason);
            } else if handle.is_trapping_exits() {
                // Send as a message
                let exit_msg = SystemMessage::exit(self.pid, reason);
                handle.send(&exit_msg)?;
            } else if reason.is_abnormal() {
                // Propagate the exit
                handle.mark_terminated(reason);
            }
            // Normal exits from other processes are ignored if not trapping
            Ok(())
        } else {
            Err(SendError::ProcessNotFound(target))
        }
    }

    /// Looks up a process by registered name.
    pub fn whereis(&self, name: &str) -> Option<Pid> {
        self.registry.whereis(name)
    }

    /// Registers a name for this process.
    ///
    /// Returns `false` if the name is already taken.
    pub fn register(&self, name: String) -> bool {
        self.registry.register_name(name, self.pid)
    }

    /// Unregisters a name.
    pub fn unregister(&self, name: &str) -> Option<Pid> {
        self.registry.unregister_name(name)
    }

    /// Returns `true` if the given process is alive.
    pub fn is_alive(&self, pid: Pid) -> bool {
        self.registry
            .get(pid)
            .map(|h| h.is_alive())
            .unwrap_or(false)
    }

    /// Returns a handle to our own process.
    pub(crate) fn handle(&self) -> ProcessHandle {
        self.registry.get(self.pid).unwrap()
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context").field("pid", &self.pid).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::MailboxSender;

    fn create_test_context(registry: &ProcessRegistry) -> (Context, MailboxSender) {
        let pid = Pid::new();
        let (mailbox, sender) = Mailbox::new();
        let state = Arc::new(RwLock::new(ProcessState::new(pid)));

        // Register the process
        let handle = ProcessHandle::new(pid, sender.clone(), state.clone(), None);
        registry.register(handle);

        let ctx = Context::new(pid, mailbox, state, registry.clone());
        (ctx, sender)
    }

    #[test]
    fn test_context_pid() {
        let registry = ProcessRegistry::new();
        let (ctx, _sender) = create_test_context(&registry);
        assert!(ctx.pid().is_local());
    }

    #[test]
    fn test_trap_exit() {
        let registry = ProcessRegistry::new();
        let (ctx, _sender) = create_test_context(&registry);

        assert!(!ctx.is_trapping_exits());

        let prev = ctx.set_trap_exit(true);
        assert!(!prev);
        assert!(ctx.is_trapping_exits());
    }

    #[tokio::test]
    async fn test_recv() {
        let registry = ProcessRegistry::new();
        let (mut ctx, sender) = create_test_context(&registry);

        // Send a message via the sender
        sender
            .send(crate::mailbox::Envelope::new(vec![1, 2, 3]))
            .unwrap();

        let msg = ctx.recv().await.unwrap();
        assert_eq!(msg, vec![1, 2, 3]);
    }

    #[test]
    fn test_link() {
        let registry = ProcessRegistry::new();
        let (ctx1, _sender1) = create_test_context(&registry);
        let (ctx2, _sender2) = create_test_context(&registry);

        ctx1.link(ctx2.pid()).unwrap();

        // Both should have links
        let state1 = ctx1.state.read().unwrap();
        assert!(state1.links.contains(&ctx2.pid()));

        let state2 = ctx2.state.read().unwrap();
        assert!(state2.links.contains(&ctx1.pid()));
    }

    #[test]
    fn test_unlink() {
        let registry = ProcessRegistry::new();
        let (ctx1, _sender1) = create_test_context(&registry);
        let (ctx2, _sender2) = create_test_context(&registry);

        ctx1.link(ctx2.pid()).unwrap();
        ctx1.unlink(ctx2.pid());

        let state1 = ctx1.state.read().unwrap();
        assert!(!state1.links.contains(&ctx2.pid()));

        let state2 = ctx2.state.read().unwrap();
        assert!(!state2.links.contains(&ctx1.pid()));
    }

    #[test]
    fn test_monitor() {
        let registry = ProcessRegistry::new();
        let (ctx1, _sender1) = create_test_context(&registry);
        let (ctx2, _sender2) = create_test_context(&registry);

        let reference = ctx1.monitor(ctx2.pid()).unwrap();

        // ctx1 should have the monitor recorded
        let state1 = ctx1.state.read().unwrap();
        assert_eq!(state1.monitors.get(&reference), Some(&ctx2.pid()));

        // ctx2 should know it's being monitored
        let state2 = ctx2.state.read().unwrap();
        assert_eq!(state2.monitored_by.get(&reference), Some(&ctx1.pid()));
    }

    #[test]
    fn test_demonitor() {
        let registry = ProcessRegistry::new();
        let (ctx1, _sender1) = create_test_context(&registry);
        let (ctx2, _sender2) = create_test_context(&registry);

        let reference = ctx1.monitor(ctx2.pid()).unwrap();
        ctx1.demonitor(reference);

        let state1 = ctx1.state.read().unwrap();
        assert!(!state1.monitors.contains_key(&reference));

        let state2 = ctx2.state.read().unwrap();
        assert!(!state2.monitored_by.contains_key(&reference));
    }

    #[test]
    fn test_register_name() {
        let registry = ProcessRegistry::new();
        let (ctx, _sender) = create_test_context(&registry);

        assert!(ctx.register("test_proc".to_string()));
        assert_eq!(ctx.whereis("test_proc"), Some(ctx.pid()));

        // Can't register same name twice
        assert!(!ctx.register("test_proc".to_string()));
    }
}
