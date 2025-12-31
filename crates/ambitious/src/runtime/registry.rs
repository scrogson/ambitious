//! Process registry for mapping PIDs to process handles.
//!
//! The [`ProcessRegistry`] provides thread-safe access to all running processes,
//! allowing message delivery, process lookup, and name registration.

use super::error::SendError;
use super::process_handle::ProcessHandle;
use crate::core::{Pid, Term};
use dashmap::DashMap;
use std::sync::{Arc, OnceLock};

/// Type alias for the remote send hook function.
///
/// This function is called when attempting to send to a non-local PID.
/// Returns `Ok(())` if the message was sent, or an error.
pub type RemoteSendHook = fn(pid: Pid, data: Vec<u8>) -> Result<(), SendError>;

/// Global hook for sending to remote processes.
///
/// Set by the distribution layer when initialized.
static REMOTE_SEND_HOOK: OnceLock<RemoteSendHook> = OnceLock::new();

/// Set the remote send hook.
///
/// This should be called by the distribution layer when it's initialized.
/// Can only be set once.
pub fn set_remote_send_hook(hook: RemoteSendHook) -> Result<(), RemoteSendHook> {
    REMOTE_SEND_HOOK.set(hook)
}

/// A thread-safe registry of all running processes.
///
/// The registry maintains mappings from:
/// - PIDs to process handles
/// - Registered names to PIDs
///
/// # Examples
///
/// ```
/// use ambitious::runtime::ProcessRegistry;
///
/// let registry = ProcessRegistry::new();
/// // Processes are registered when spawned via the runtime
/// ```
#[derive(Clone)]
pub struct ProcessRegistry {
    /// Map from PID to process handle.
    processes: Arc<DashMap<Pid, ProcessHandle>>,
    /// Map from registered name to PID.
    names: Arc<DashMap<String, Pid>>,
}

impl ProcessRegistry {
    /// Creates a new empty registry.
    pub fn new() -> Self {
        Self {
            processes: Arc::new(DashMap::new()),
            names: Arc::new(DashMap::new()),
        }
    }

    /// Registers a process in the registry.
    pub fn register(&self, handle: ProcessHandle) {
        self.processes.insert(handle.pid(), handle);
    }

    /// Removes a process from the registry.
    ///
    /// Also removes any name registrations for this process.
    pub fn unregister(&self, pid: Pid) -> Option<ProcessHandle> {
        // Remove any name registrations
        self.names.retain(|_, &mut p| p != pid);
        // Remove from processes
        self.processes.remove(&pid).map(|(_, h)| h)
    }

    /// Gets a handle to a process by PID.
    pub fn get(&self, pid: Pid) -> Option<ProcessHandle> {
        self.processes.get(&pid).map(|r| r.value().clone())
    }

    /// Returns `true` if a process with the given PID exists.
    pub fn contains(&self, pid: Pid) -> bool {
        self.processes.contains_key(&pid)
    }

    /// Returns the number of registered processes.
    pub fn len(&self) -> usize {
        self.processes.len()
    }

    /// Returns `true` if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    /// Sends a raw message to a process.
    ///
    /// If the PID refers to a remote process and distribution is configured,
    /// the message will be routed through the distribution layer.
    pub fn send_raw(&self, pid: Pid, data: Vec<u8>) -> Result<(), SendError> {
        // Check if this is a remote PID
        if !pid.is_local() {
            // Try to send via distribution layer
            if let Some(hook) = REMOTE_SEND_HOOK.get() {
                return hook(pid, data);
            } else {
                // Distribution not configured - can't send to remote
                return Err(SendError::ProcessNotFound(pid));
            }
        }

        // Local PID - send directly
        match self.processes.get(&pid) {
            Some(handle) => handle.send_raw(data),
            None => Err(SendError::ProcessNotFound(pid)),
        }
    }

    /// Sends a typed message to a process.
    ///
    /// For local and native remote processes, uses postcard encoding.
    /// For BEAM/Erlang nodes (when `erlang-dist` feature is enabled),
    /// automatically encodes as ETF for transparent interoperability.
    #[cfg(feature = "erlang-dist")]
    pub fn send<M: Term + serde::Serialize>(&self, pid: Pid, msg: &M) -> Result<(), SendError> {
        // Check if this is a BEAM node - if so, encode as ETF
        if !pid.is_local() && crate::distribution::is_beam_node(pid.node()) {
            let etf_bytes = erltf_serde::to_bytes(msg)
                .map_err(|e| SendError::EncodingError(format!("ETF encoding failed: {}", e)))?;
            return crate::distribution::send_to_beam(pid, etf_bytes)
                .map_err(|e| SendError::DistributionError(e.to_string()));
        }

        // Default: postcard encoding for local and native remote processes
        self.send_raw(pid, msg.encode())
    }

    /// Sends a typed message to a process.
    #[cfg(not(feature = "erlang-dist"))]
    pub fn send<M: Term>(&self, pid: Pid, msg: &M) -> Result<(), SendError> {
        self.send_raw(pid, msg.encode())
    }

    /// Registers a name for a process.
    ///
    /// Returns `false` if the name is already registered.
    pub fn register_name(&self, name: String, pid: Pid) -> bool {
        if self.names.contains_key(&name) {
            return false;
        }
        self.names.insert(name, pid);
        true
    }

    /// Looks up a process by registered name.
    pub fn whereis(&self, name: &str) -> Option<Pid> {
        self.names.get(name).map(|r| *r.value())
    }

    /// Unregisters a name.
    ///
    /// Returns the PID that was registered, if any.
    pub fn unregister_name(&self, name: &str) -> Option<Pid> {
        self.names.remove(name).map(|(_, pid)| pid)
    }

    /// Returns all registered names.
    pub fn registered_names(&self) -> Vec<String> {
        self.names.iter().map(|r| r.key().clone()).collect()
    }

    /// Returns all process PIDs.
    pub fn pids(&self) -> Vec<Pid> {
        self.processes.iter().map(|r| *r.key()).collect()
    }

    /// Iterates over all processes, calling the provided function.
    pub fn for_each<F>(&self, f: F)
    where
        F: FnMut(ProcessHandle),
    {
        self.processes.iter().map(|r| r.value().clone()).for_each(f);
    }
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ProcessRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessRegistry")
            .field("process_count", &self.processes.len())
            .field("name_count", &self.names.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Mailbox;
    use crate::runtime::ProcessState;
    use std::sync::RwLock;

    fn create_test_handle(pid: Pid) -> ProcessHandle {
        let (_mailbox, sender) = Mailbox::new();
        let state = Arc::new(RwLock::new(ProcessState::new(pid)));
        ProcessHandle::new(pid, sender, state, None)
    }

    #[test]
    fn test_register_and_get() {
        let registry = ProcessRegistry::new();
        let pid = Pid::new();
        let handle = create_test_handle(pid);

        registry.register(handle);

        assert!(registry.contains(pid));
        assert_eq!(registry.len(), 1);

        let retrieved = registry.get(pid).unwrap();
        assert_eq!(retrieved.pid(), pid);
    }

    #[test]
    fn test_unregister() {
        let registry = ProcessRegistry::new();
        let pid = Pid::new();
        let handle = create_test_handle(pid);

        registry.register(handle);
        assert!(registry.contains(pid));

        let removed = registry.unregister(pid);
        assert!(removed.is_some());
        assert!(!registry.contains(pid));
        assert!(registry.is_empty());
    }

    #[test]
    fn test_name_registration() {
        let registry = ProcessRegistry::new();
        let pid = Pid::new();
        let handle = create_test_handle(pid);

        registry.register(handle);

        // Register a name
        assert!(registry.register_name("my_process".to_string(), pid));

        // Can look up by name
        assert_eq!(registry.whereis("my_process"), Some(pid));

        // Can't register the same name twice
        let pid2 = Pid::new();
        assert!(!registry.register_name("my_process".to_string(), pid2));

        // Unregister the name
        assert_eq!(registry.unregister_name("my_process"), Some(pid));
        assert_eq!(registry.whereis("my_process"), None);
    }

    #[test]
    fn test_unregister_removes_names() {
        let registry = ProcessRegistry::new();
        let pid = Pid::new();
        let handle = create_test_handle(pid);

        registry.register(handle);
        registry.register_name("my_process".to_string(), pid);

        // Unregistering the process should also remove the name
        registry.unregister(pid);

        assert_eq!(registry.whereis("my_process"), None);
    }

    #[test]
    fn test_pids_and_names() {
        let registry = ProcessRegistry::new();

        let pid1 = Pid::new();
        let pid2 = Pid::new();

        registry.register(create_test_handle(pid1));
        registry.register(create_test_handle(pid2));
        registry.register_name("proc1".to_string(), pid1);
        registry.register_name("proc2".to_string(), pid2);

        let pids = registry.pids();
        assert_eq!(pids.len(), 2);
        assert!(pids.contains(&pid1));
        assert!(pids.contains(&pid2));

        let names = registry.registered_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"proc1".to_string()));
        assert!(names.contains(&"proc2".to_string()));
    }
}
