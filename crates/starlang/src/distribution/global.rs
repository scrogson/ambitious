//! Global process registry for distributed Starlang.
//!
//! Allows processes to be registered by name across all connected nodes.
//! Similar to Erlang's `:global` module.
//!
//! # How it works
//!
//! - When a process is registered globally, it's announced to all connected nodes
//! - Each node maintains a local cache of global registrations
//! - Lookups check the local cache first
//! - The owning node is authoritative for a name
//!
//! # PID Routing
//!
//! PIDs now contain the node name as an Atom, so they are globally unambiguous.
//! No PID rewriting is needed - a PID like `<node2@localhost.5.0>` means the
//! same thing on any node.

use super::DIST_MANAGER;
use super::protocol::DistMessage;
use crate::core::{Atom, Pid};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Global registry instance.
static GLOBAL_REGISTRY: OnceLock<GlobalRegistry> = OnceLock::new();

/// Message types for global registry synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GlobalRegistryMessage {
    /// Register a name globally.
    Register {
        /// The name to register.
        name: String,
        /// The PID to register under the name.
        pid: Pid,
    },
    /// Unregister a name.
    Unregister {
        /// The name to unregister.
        name: String,
    },
    /// Request the full registry (sent on connect).
    SyncRequest,
    /// Full registry sync response.
    SyncResponse {
        /// The entries in the registry.
        entries: Vec<(String, Pid)>,
    },
}

/// The global process registry.
pub struct GlobalRegistry {
    /// Name -> PID mapping.
    names: DashMap<String, Pid>,
}

impl GlobalRegistry {
    /// Create a new global registry.
    pub fn new() -> Self {
        Self {
            names: DashMap::new(),
        }
    }

    /// Register a process globally.
    ///
    /// Returns `false` if the name is already registered.
    pub fn register(&self, name: String, pid: Pid) -> bool {
        // Check if already registered
        if self.names.contains_key(&name) {
            return false;
        }

        self.names.insert(name.clone(), pid);

        // Broadcast to all connected nodes
        self.broadcast_register(&name, pid);

        true
    }

    /// Unregister a global name.
    pub fn unregister(&self, name: &str) -> Option<Pid> {
        let result = self.names.remove(name).map(|(_, pid)| pid);

        if result.is_some() {
            // Broadcast unregister to all nodes
            self.broadcast_unregister(name);
        }

        result
    }

    /// Look up a process by global name.
    pub fn whereis(&self, name: &str) -> Option<Pid> {
        self.names.get(name).map(|r| *r.value())
    }

    /// Get all registered names.
    pub fn registered(&self) -> Vec<String> {
        self.names.iter().map(|r| r.key().clone()).collect()
    }

    /// Handle an incoming global registry message from another node.
    ///
    /// PIDs are now globally unambiguous (they contain the node name as an Atom),
    /// so no rewriting is needed.
    pub fn handle_message(&self, msg: GlobalRegistryMessage, from_node: Atom) {
        match msg {
            GlobalRegistryMessage::Register { name, pid } => {
                // PIDs are now globally unambiguous - no rewriting needed
                tracing::debug!(%name, ?pid, from_node = %from_node, "Received global register");
                // Only accept if not already registered locally with a different PID
                self.names.entry(name).or_insert(pid);
            }
            GlobalRegistryMessage::Unregister { name } => {
                tracing::debug!(%name, from_node = %from_node, "Received global unregister");
                self.names.remove(&name);
            }
            GlobalRegistryMessage::SyncRequest => {
                tracing::debug!(from_node = %from_node, "Received global sync request");
                // Send our registrations to the requesting node
                let entries: Vec<(String, Pid)> = self
                    .names
                    .iter()
                    .map(|r| (r.key().clone(), *r.value()))
                    .collect();

                if let Some(manager) = DIST_MANAGER.get() {
                    if let Some(tx) = manager.get_node_tx(from_node) {
                        let msg = GlobalRegistryMessage::SyncResponse { entries };
                        if let Ok(payload) = postcard::to_allocvec(&msg) {
                            let _ = tx.try_send(DistMessage::GlobalRegistry { payload });
                        }
                    }
                }
            }
            GlobalRegistryMessage::SyncResponse { entries } => {
                tracing::debug!(count = entries.len(), from_node = %from_node, "Received global sync response");
                // Merge remote registrations into our cache
                // PIDs are globally unambiguous - no rewriting needed
                for (name, pid) in entries {
                    tracing::debug!(%name, ?pid, "Merging global entry");
                    self.names.entry(name).or_insert(pid);
                }
            }
        }
    }

    /// Broadcast a registration to all connected nodes.
    fn broadcast_register(&self, name: &str, pid: Pid) {
        let msg = GlobalRegistryMessage::Register {
            name: name.to_string(),
            pid,
        };
        self.broadcast_global_message(&msg);
    }

    /// Broadcast an unregistration to all connected nodes.
    fn broadcast_unregister(&self, name: &str) {
        let msg = GlobalRegistryMessage::Unregister {
            name: name.to_string(),
        };
        self.broadcast_global_message(&msg);
    }

    /// Broadcast a global registry message to all connected nodes.
    fn broadcast_global_message(&self, msg: &GlobalRegistryMessage) {
        if let Some(manager) = DIST_MANAGER.get() {
            if let Ok(payload) = postcard::to_allocvec(msg) {
                let dist_msg = DistMessage::GlobalRegistry { payload };
                for node_atom in manager.connected_nodes() {
                    if let Some(tx) = manager.get_node_tx(node_atom) {
                        let _ = tx.try_send(dist_msg.clone());
                    }
                }
            }
        }
    }

    /// Request sync from a newly connected node.
    pub fn request_sync(&self, node_atom: Atom) {
        if let Some(manager) = DIST_MANAGER.get() {
            if let Some(tx) = manager.get_node_tx(node_atom) {
                // Ask them to send their registry to us
                let msg = GlobalRegistryMessage::SyncRequest;
                if let Ok(payload) = postcard::to_allocvec(&msg) {
                    let _ = tx.try_send(DistMessage::GlobalRegistry { payload });
                }

                // Also push our registry to them
                let entries: Vec<(String, Pid)> = self
                    .names
                    .iter()
                    .map(|r| (r.key().clone(), *r.value()))
                    .collect();

                if !entries.is_empty() {
                    let response = GlobalRegistryMessage::SyncResponse { entries };
                    if let Ok(payload) = postcard::to_allocvec(&response) {
                        let _ = tx.try_send(DistMessage::GlobalRegistry { payload });
                    }
                }
            }
        }
    }
}

impl Default for GlobalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Get or initialize the global registry.
pub fn global_registry() -> &'static GlobalRegistry {
    GLOBAL_REGISTRY.get_or_init(GlobalRegistry::new)
}

// === Public API ===

/// Register a process globally by name.
///
/// The process will be visible to all connected nodes.
/// Returns `false` if the name is already registered.
///
/// # Example
///
/// ```ignore
/// let pid = starlang::spawn(|| async { /* ... */ });
/// starlang::dist::global::register("my_service", pid);
/// ```
pub fn register(name: impl Into<String>, pid: Pid) -> bool {
    global_registry().register(name.into(), pid)
}

/// Unregister a global name.
pub fn unregister(name: &str) -> Option<Pid> {
    global_registry().unregister(name)
}

/// Look up a process by global name.
///
/// Returns the PID if found, regardless of which node owns it.
///
/// # Example
///
/// ```ignore
/// if let Some(pid) = starlang::dist::global::whereis("my_service") {
///     starlang::send_raw(pid, message);  // Works even if remote!
/// }
/// ```
pub fn whereis(name: &str) -> Option<Pid> {
    global_registry().whereis(name)
}

/// Get all globally registered names.
pub fn registered() -> Vec<String> {
    global_registry().registered()
}
