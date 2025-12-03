//! Process handle for interacting with running processes.
//!
//! A [`ProcessHandle`] provides an interface for sending messages to a process,
//! managing links and monitors, and querying process state.

use crate::mailbox::{Envelope, MailboxSender};
use crate::SendError;
use dream_core::{ExitReason, Pid, Ref, Term};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;

/// Internal state shared between the process and its handle.
#[derive(Debug)]
pub struct ProcessState {
    /// The process identifier.
    pub pid: Pid,
    /// Whether the process is trapping exits.
    pub trap_exit: bool,
    /// Processes linked to this one (bidirectional).
    pub links: HashSet<Pid>,
    /// Monitors this process has created (ref -> monitored pid).
    pub monitors: std::collections::HashMap<Ref, Pid>,
    /// Processes monitoring this one (ref -> monitoring pid).
    pub monitored_by: std::collections::HashMap<Ref, Pid>,
    /// Whether the process has terminated.
    pub terminated: bool,
    /// The exit reason if terminated.
    pub exit_reason: Option<ExitReason>,
}

impl ProcessState {
    /// Creates a new process state.
    pub fn new(pid: Pid) -> Self {
        Self {
            pid,
            trap_exit: false,
            links: HashSet::new(),
            monitors: std::collections::HashMap::new(),
            monitored_by: std::collections::HashMap::new(),
            terminated: false,
            exit_reason: None,
        }
    }
}

/// A handle to a running process.
///
/// This handle can be cloned and shared between threads. It provides
/// methods for sending messages and managing process relationships.
#[derive(Clone)]
pub struct ProcessHandle {
    /// The process identifier.
    pid: Pid,
    /// Channel for sending messages to the process.
    sender: MailboxSender,
    /// Shared process state.
    state: Arc<RwLock<ProcessState>>,
    /// Channel to signal process termination (for joining).
    #[allow(dead_code)]
    termination_tx: Arc<Option<oneshot::Sender<ExitReason>>>,
}

impl ProcessHandle {
    /// Creates a new process handle.
    pub fn new(
        pid: Pid,
        sender: MailboxSender,
        state: Arc<RwLock<ProcessState>>,
        termination_tx: Option<oneshot::Sender<ExitReason>>,
    ) -> Self {
        Self {
            pid,
            sender,
            state,
            termination_tx: Arc::new(termination_tx),
        }
    }

    /// Returns the process identifier.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Sends a raw message (bytes) to the process.
    pub fn send_raw(&self, data: Vec<u8>) -> Result<(), SendError> {
        if self.sender.is_closed() {
            return Err(SendError::ProcessTerminated);
        }
        self.sender
            .send(Envelope::new(data))
            .map_err(|_| SendError::ProcessTerminated)
    }

    /// Sends a typed message to the process.
    pub fn send<M: Term>(&self, msg: &M) -> Result<(), SendError> {
        self.send_raw(msg.encode())
    }

    /// Returns `true` if the process is still alive.
    pub fn is_alive(&self) -> bool {
        let state = self.state.read().unwrap();
        !state.terminated
    }

    /// Returns `true` if the process is trapping exits.
    pub fn is_trapping_exits(&self) -> bool {
        let state = self.state.read().unwrap();
        state.trap_exit
    }

    /// Sets the trap_exit flag.
    pub fn set_trap_exit(&self, trap: bool) {
        let mut state = self.state.write().unwrap();
        state.trap_exit = trap;
    }

    /// Adds a link to another process.
    ///
    /// Links are bidirectional - this only updates our side.
    /// The caller must also update the other process.
    pub fn add_link(&self, other: Pid) {
        let mut state = self.state.write().unwrap();
        state.links.insert(other);
    }

    /// Removes a link to another process.
    pub fn remove_link(&self, other: Pid) {
        let mut state = self.state.write().unwrap();
        state.links.remove(&other);
    }

    /// Returns all linked processes.
    pub fn links(&self) -> Vec<Pid> {
        let state = self.state.read().unwrap();
        state.links.iter().copied().collect()
    }

    /// Adds a monitor (we are monitoring `target`).
    pub fn add_monitor(&self, reference: Ref, target: Pid) {
        let mut state = self.state.write().unwrap();
        state.monitors.insert(reference, target);
    }

    /// Removes a monitor.
    pub fn remove_monitor(&self, reference: Ref) -> Option<Pid> {
        let mut state = self.state.write().unwrap();
        state.monitors.remove(&reference)
    }

    /// Adds a process that is monitoring us.
    pub fn add_monitored_by(&self, reference: Ref, monitoring_pid: Pid) {
        let mut state = self.state.write().unwrap();
        state.monitored_by.insert(reference, monitoring_pid);
    }

    /// Removes a process from our monitored_by set.
    pub fn remove_monitored_by(&self, reference: Ref) -> Option<Pid> {
        let mut state = self.state.write().unwrap();
        state.monitored_by.remove(&reference)
    }

    /// Returns all processes monitoring this one.
    pub fn monitored_by(&self) -> Vec<(Ref, Pid)> {
        let state = self.state.read().unwrap();
        state.monitored_by.iter().map(|(r, p)| (*r, *p)).collect()
    }

    /// Marks the process as terminated.
    pub fn mark_terminated(&self, reason: ExitReason) {
        let mut state = self.state.write().unwrap();
        state.terminated = true;
        state.exit_reason = Some(reason);
    }

    /// Returns the exit reason if the process has terminated.
    pub fn exit_reason(&self) -> Option<ExitReason> {
        let state = self.state.read().unwrap();
        state.exit_reason.clone()
    }
}

impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessHandle")
            .field("pid", &self.pid)
            .field("alive", &self.is_alive())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::Mailbox;

    fn create_test_handle() -> (ProcessHandle, crate::mailbox::Mailbox) {
        let pid = Pid::new();
        let (mailbox, sender) = Mailbox::new();
        let state = Arc::new(RwLock::new(ProcessState::new(pid)));
        let handle = ProcessHandle::new(pid, sender, state, None);
        (handle, mailbox)
    }

    #[test]
    fn test_process_handle_pid() {
        let (handle, _mailbox) = create_test_handle();
        let pid = handle.pid();
        assert!(pid.is_local());
    }

    #[tokio::test]
    async fn test_send_message() {
        let (handle, mut mailbox) = create_test_handle();

        handle.send_raw(vec![1, 2, 3]).unwrap();

        let envelope = mailbox.recv().await.unwrap();
        assert_eq!(envelope.data, vec![1, 2, 3]);
    }

    #[test]
    fn test_trap_exit() {
        let (handle, _mailbox) = create_test_handle();

        assert!(!handle.is_trapping_exits());

        handle.set_trap_exit(true);
        assert!(handle.is_trapping_exits());

        handle.set_trap_exit(false);
        assert!(!handle.is_trapping_exits());
    }

    #[test]
    fn test_links() {
        let (handle, _mailbox) = create_test_handle();
        let other_pid = Pid::new();

        assert!(handle.links().is_empty());

        handle.add_link(other_pid);
        assert_eq!(handle.links(), vec![other_pid]);

        handle.remove_link(other_pid);
        assert!(handle.links().is_empty());
    }

    #[test]
    fn test_monitors() {
        let (handle, _mailbox) = create_test_handle();
        let target_pid = Pid::new();
        let reference = Ref::new();

        handle.add_monitor(reference, target_pid);

        let removed = handle.remove_monitor(reference);
        assert_eq!(removed, Some(target_pid));

        let removed_again = handle.remove_monitor(reference);
        assert_eq!(removed_again, None);
    }

    #[test]
    fn test_terminated() {
        let (handle, _mailbox) = create_test_handle();

        assert!(handle.is_alive());
        assert!(handle.exit_reason().is_none());

        handle.mark_terminated(ExitReason::Normal);

        assert!(!handle.is_alive());
        assert_eq!(handle.exit_reason(), Some(ExitReason::Normal));
    }
}
