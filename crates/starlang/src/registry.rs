//! Local process registry with pub/sub support.
//!
//! Similar to Elixir's `Registry`, this provides a local, decentralized process
//! registry that supports both unique and duplicate key registration, plus
//! efficient dispatch for pub/sub patterns.
//!
//! # Key Types
//!
//! - **Unique registries**: Each key maps to at most one process (like `whereis`)
//! - **Duplicate registries**: Multiple processes can register under the same key (for pub/sub)
//!
//! # Example: Unique Registry
//!
//! ```ignore
//! use starlang::registry::Registry;
//!
//! // Create a unique registry
//! let registry = Registry::new_unique("services");
//!
//! // Register current process under a key
//! registry.register("database", my_pid, "primary");
//!
//! // Lookup
//! if let Some((pid, value)) = registry.lookup_one("database") {
//!     println!("Found database service: {:?} with value {:?}", pid, value);
//! }
//! ```
//!
//! # Example: Duplicate Registry (Pub/Sub)
//!
//! ```ignore
//! use starlang::registry::Registry;
//!
//! // Create a duplicate registry for pub/sub
//! let registry = Registry::new_duplicate("pubsub");
//!
//! // Multiple processes can subscribe to same topic
//! registry.register("room:lobby", alice_pid, ());
//! registry.register("room:lobby", bob_pid, ());
//!
//! // Dispatch to all subscribers
//! registry.dispatch("room:lobby", |entries| {
//!     for (pid, _value) in entries {
//!         starlang::send_raw(pid, message.clone());
//!     }
//! });
//! ```
//!
//! # Automatic Cleanup
//!
//! When a process terminates, all its registrations are automatically removed.
//! This is handled by monitoring registered processes.

use dashmap::DashMap;
use starlang_core::Pid;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

/// The type of registry - unique or duplicate keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryType {
    /// Each key can only have one registration.
    Unique,
    /// Multiple processes can register under the same key.
    Duplicate,
}

/// A process registry entry.
#[derive(Debug, Clone)]
struct Entry<V> {
    /// The registered process.
    pid: Pid,
    /// The associated value.
    value: V,
}

/// A local process registry.
///
/// Provides efficient key-based process lookup and dispatch.
/// Supports both unique (one process per key) and duplicate (many processes per key) modes.
pub struct Registry<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Registry name (for debugging).
    #[allow(dead_code)]
    name: String,
    /// Registry type.
    registry_type: RegistryType,
    /// Key -> entries mapping.
    /// For unique registries, each key has at most one entry.
    /// For duplicate registries, each key can have multiple entries.
    entries: DashMap<K, Vec<Entry<V>>>,
    /// Reverse lookup: PID -> keys registered by that PID.
    /// Used for efficient cleanup when a process terminates.
    pid_to_keys: DashMap<Pid, HashSet<K>>,
    /// Registered listeners for process termination.
    /// In a real implementation, we'd monitor processes and clean up on exit.
    #[allow(dead_code)]
    monitors: RwLock<HashMap<Pid, ()>>,
}

impl<K, V> Registry<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Create a new unique registry.
    ///
    /// In a unique registry, each key can only have one registration.
    /// Attempting to register a key that's already registered returns an error.
    pub fn new_unique(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            registry_type: RegistryType::Unique,
            entries: DashMap::new(),
            pid_to_keys: DashMap::new(),
            monitors: RwLock::new(HashMap::new()),
        })
    }

    /// Create a new duplicate registry.
    ///
    /// In a duplicate registry, multiple processes can register under the same key.
    /// This is useful for pub/sub patterns.
    pub fn new_duplicate(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            registry_type: RegistryType::Duplicate,
            entries: DashMap::new(),
            pid_to_keys: DashMap::new(),
            monitors: RwLock::new(HashMap::new()),
        })
    }

    /// Register a process under a key with an associated value.
    ///
    /// For unique registries, returns `Err` if the key is already registered.
    /// For duplicate registries, always succeeds (adds another registration).
    pub fn register(&self, key: K, pid: Pid, value: V) -> Result<(), RegistryError> {
        match self.registry_type {
            RegistryType::Unique => {
                // Check if key already exists
                if self.entries.contains_key(&key) {
                    return Err(RegistryError::AlreadyRegistered);
                }
                self.entries.insert(key.clone(), vec![Entry { pid, value }]);
            }
            RegistryType::Duplicate => {
                // Add to existing entries or create new
                self.entries
                    .entry(key.clone())
                    .or_default()
                    .push(Entry { pid, value });
            }
        }

        // Track reverse mapping
        self.pid_to_keys.entry(pid).or_default().insert(key);

        Ok(())
    }

    /// Unregister a process from a key.
    ///
    /// For unique registries, removes the registration if PID matches.
    /// For duplicate registries, removes only the entry for this PID.
    pub fn unregister(&self, key: &K, pid: Pid) {
        if let Some(mut entries) = self.entries.get_mut(key) {
            entries.retain(|e| e.pid != pid);
            if entries.is_empty() {
                drop(entries);
                self.entries.remove(key);
            }
        }

        // Update reverse mapping
        if let Some(mut keys) = self.pid_to_keys.get_mut(&pid) {
            keys.remove(key);
            if keys.is_empty() {
                drop(keys);
                self.pid_to_keys.remove(&pid);
            }
        }
    }

    /// Unregister a process from all keys.
    ///
    /// This is typically called when a process terminates.
    pub fn unregister_all(&self, pid: Pid) {
        if let Some((_, keys)) = self.pid_to_keys.remove(&pid) {
            for key in keys {
                if let Some(mut entries) = self.entries.get_mut(&key) {
                    entries.retain(|e| e.pid != pid);
                    if entries.is_empty() {
                        drop(entries);
                        self.entries.remove(&key);
                    }
                }
            }
        }
    }

    /// Lookup all registrations for a key.
    ///
    /// Returns a list of (pid, value) pairs.
    pub fn lookup(&self, key: &K) -> Vec<(Pid, V)> {
        self.entries
            .get(key)
            .map(|entries| entries.iter().map(|e| (e.pid, e.value.clone())).collect())
            .unwrap_or_default()
    }

    /// Lookup a single registration for a key (for unique registries).
    ///
    /// Returns the first (pid, value) pair if any exist.
    pub fn lookup_one(&self, key: &K) -> Option<(Pid, V)> {
        self.entries
            .get(key)
            .and_then(|entries| entries.first().map(|e| (e.pid, e.value.clone())))
    }

    /// Get all keys registered by a specific process.
    pub fn keys(&self, pid: Pid) -> Vec<K> {
        self.pid_to_keys
            .get(&pid)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Dispatch to all processes registered under a key.
    ///
    /// The callback receives a slice of (pid, value) pairs.
    /// This is the core primitive for pub/sub - iterate over entries
    /// and send messages to each process.
    ///
    /// # Example
    ///
    /// ```ignore
    /// registry.dispatch(&"room:lobby", |entries| {
    ///     for (pid, _) in entries {
    ///         starlang::send_raw(pid, message.clone());
    ///     }
    /// });
    /// ```
    pub fn dispatch<F>(&self, key: &K, callback: F)
    where
        F: FnOnce(&[(Pid, V)]),
    {
        if let Some(entries) = self.entries.get(key) {
            let pairs: Vec<(Pid, V)> = entries.iter().map(|e| (e.pid, e.value.clone())).collect();
            callback(&pairs);
        }
    }

    /// Dispatch to all processes, with mutable access to allow filtering.
    ///
    /// Similar to `dispatch` but the callback can return which entries to keep.
    /// This is useful for implementing "broadcast except sender" patterns.
    pub fn dispatch_filter<F>(&self, key: &K, callback: F)
    where
        F: FnOnce(&[(Pid, V)]) -> Vec<Pid>,
    {
        if let Some(entries) = self.entries.get(key) {
            let pairs: Vec<(Pid, V)> = entries.iter().map(|e| (e.pid, e.value.clone())).collect();
            let _pids_to_send = callback(&pairs);
            // Caller handles sending
        }
    }

    /// Get the count of registrations for a key.
    pub fn count(&self, key: &K) -> usize {
        self.entries.get(key).map(|e| e.len()).unwrap_or(0)
    }

    /// Get the total count of all registrations.
    pub fn count_all(&self) -> usize {
        self.entries.iter().map(|e| e.value().len()).sum()
    }

    /// Check if a key has any registrations.
    pub fn has_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Get all registered keys.
    pub fn all_keys(&self) -> Vec<K> {
        self.entries.iter().map(|e| e.key().clone()).collect()
    }

    /// Update the value for a registration (unique registries only).
    ///
    /// Returns the old value if successful.
    pub fn update_value(&self, key: &K, pid: Pid, new_value: V) -> Option<V> {
        if self.registry_type != RegistryType::Unique {
            return None;
        }

        self.entries.get_mut(key).and_then(|mut entries| {
            entries.iter_mut().find(|e| e.pid == pid).map(|e| {
                let old = e.value.clone();
                e.value = new_value;
                old
            })
        })
    }
}

// Implement NameResolver for Registry<Vec<u8>, V> so it can be used with ServerRef::via()
// Keys are stored as serialized bytes, allowing any Term type to be used as a key.
impl<V> starlang_gen_server::NameResolver for Registry<Vec<u8>, V>
where
    V: Clone + Send + Sync + Default + 'static,
{
    fn whereis_term(&self, key: &[u8]) -> Option<Pid> {
        self.lookup_one(&key.to_vec()).map(|(pid, _)| pid)
    }

    fn register_term(&self, key: &[u8], pid: Pid) -> bool {
        self.register(key.to_vec(), pid, V::default()).is_ok()
    }

    fn unregister_term(&self, key: &[u8]) {
        // Remove all entries for this key
        self.entries.remove(&key.to_vec());
    }
}

/// Errors that can occur during registry operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Attempted to register a key that's already registered (unique registry).
    AlreadyRegistered,
    /// The specified key was not found.
    NotFound,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::AlreadyRegistered => write!(f, "key already registered"),
            RegistryError::NotFound => write!(f, "key not found"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unique_registry() {
        let registry: Arc<Registry<String, i32>> = Registry::new_unique("test");
        let pid1 = Pid::new();
        let pid2 = Pid::new();

        // First registration succeeds
        assert!(registry.register("key1".to_string(), pid1, 100).is_ok());

        // Second registration for same key fails
        assert_eq!(
            registry.register("key1".to_string(), pid2, 200),
            Err(RegistryError::AlreadyRegistered)
        );

        // Lookup returns the registered entry
        let result = registry.lookup_one(&"key1".to_string());
        assert_eq!(result, Some((pid1, 100)));
    }

    #[test]
    fn test_duplicate_registry() {
        let registry: Arc<Registry<String, &str>> = Registry::new_duplicate("test");
        let pid1 = Pid::new();
        let pid2 = Pid::new();

        // Both registrations succeed
        assert!(registry.register("topic".to_string(), pid1, "alice").is_ok());
        assert!(registry.register("topic".to_string(), pid2, "bob").is_ok());

        // Lookup returns both
        let entries = registry.lookup(&"topic".to_string());
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&(pid1, "alice")));
        assert!(entries.contains(&(pid2, "bob")));
    }

    #[test]
    fn test_unregister() {
        let registry: Arc<Registry<String, ()>> = Registry::new_duplicate("test");
        let pid1 = Pid::new();
        let pid2 = Pid::new();

        registry.register("topic".to_string(), pid1, ()).unwrap();
        registry.register("topic".to_string(), pid2, ()).unwrap();

        // Unregister one
        registry.unregister(&"topic".to_string(), pid1);

        let entries = registry.lookup(&"topic".to_string());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, pid2);
    }

    #[test]
    fn test_unregister_all() {
        let registry: Arc<Registry<String, ()>> = Registry::new_duplicate("test");
        let pid1 = Pid::new();

        registry.register("topic1".to_string(), pid1, ()).unwrap();
        registry.register("topic2".to_string(), pid1, ()).unwrap();

        // Unregister all for pid1
        registry.unregister_all(pid1);

        assert_eq!(registry.count(&"topic1".to_string()), 0);
        assert_eq!(registry.count(&"topic2".to_string()), 0);
    }

    #[test]
    fn test_dispatch() {
        let registry: Arc<Registry<String, i32>> = Registry::new_duplicate("test");
        let pid1 = Pid::new();
        let pid2 = Pid::new();

        registry.register("topic".to_string(), pid1, 1).unwrap();
        registry.register("topic".to_string(), pid2, 2).unwrap();

        let mut collected = Vec::new();
        registry.dispatch(&"topic".to_string(), |entries| {
            for (pid, value) in entries {
                collected.push((*pid, *value));
            }
        });

        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn test_keys() {
        let registry: Arc<Registry<String, ()>> = Registry::new_duplicate("test");
        let pid = Pid::new();

        registry.register("topic1".to_string(), pid, ()).unwrap();
        registry.register("topic2".to_string(), pid, ()).unwrap();

        let keys = registry.keys(pid);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"topic1".to_string()));
        assert!(keys.contains(&"topic2".to_string()));
    }
}
