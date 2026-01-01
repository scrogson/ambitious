//! Global store registry for tracking stores and their owners.
//!
//! This module maintains the mapping between stores, their owners, and
//! registered names. It's used internally for store lifecycle management.

use super::StoreId;
use crate::core::{ExitReason, Pid};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::HashSet;

/// Metadata about a registered store.
#[derive(Debug)]
struct StoreMetadata {
    /// The owning process.
    owner: Pid,
    /// Optional registered name.
    name: Option<String>,
    /// Optional heir process.
    heir: Option<Pid>,
}

/// Global registry for all stores.
struct StoreRegistry {
    /// Map from store ID to metadata.
    stores: DashMap<StoreId, StoreMetadata>,
    /// Map from registered name to store ID.
    names: DashMap<String, StoreId>,
    /// Map from owner PID to set of owned store IDs.
    by_owner: DashMap<Pid, HashSet<StoreId>>,
}

impl StoreRegistry {
    fn new() -> Self {
        Self {
            stores: DashMap::new(),
            names: DashMap::new(),
            by_owner: DashMap::new(),
        }
    }
}

/// Global store registry instance.
static REGISTRY: Lazy<StoreRegistry> = Lazy::new(StoreRegistry::new);

/// Registers a new store in the registry.
pub(crate) fn register_store(id: StoreId, owner: Pid, name: Option<String>, heir: Option<Pid>) {
    // Register the store metadata
    REGISTRY.stores.insert(
        id,
        StoreMetadata {
            owner,
            name: name.clone(),
            heir,
        },
    );

    // Register the name if provided
    if let Some(ref n) = name {
        REGISTRY.names.insert(n.clone(), id);
    }

    // Add to owner's set of stores
    REGISTRY.by_owner.entry(owner).or_default().insert(id);
}

/// Unregisters a store from the registry.
pub(crate) fn unregister_store(id: StoreId) {
    if let Some((_, metadata)) = REGISTRY.stores.remove(&id) {
        // Remove from names
        if let Some(name) = metadata.name {
            REGISTRY.names.remove(&name);
        }

        // Remove from owner's set
        if let Some(mut stores) = REGISTRY.by_owner.get_mut(&metadata.owner) {
            stores.remove(&id);
        }
    }
}

/// Transfers ownership of a store to a new owner.
pub(crate) fn transfer_ownership(id: StoreId, old_owner: Pid, new_owner: Pid) {
    // Update the metadata
    if let Some(mut metadata) = REGISTRY.stores.get_mut(&id) {
        metadata.owner = new_owner;
    }

    // Remove from old owner's set
    if let Some(mut stores) = REGISTRY.by_owner.get_mut(&old_owner) {
        stores.remove(&id);
    }

    // Add to new owner's set
    REGISTRY.by_owner.entry(new_owner).or_default().insert(id);
}

/// Looks up a store by name.
pub fn whereis(name: &str) -> Option<StoreId> {
    REGISTRY.names.get(name).map(|r| *r.value())
}

/// Returns all store IDs owned by a process.
pub fn stores_owned_by(pid: Pid) -> HashSet<StoreId> {
    REGISTRY
        .by_owner
        .get(&pid)
        .map(|r| r.value().clone())
        .unwrap_or_default()
}

/// Gets the owner of a store.
#[allow(dead_code)] // Useful API, may be used later
pub fn owner_of(id: StoreId) -> Option<Pid> {
    REGISTRY.stores.get(&id).map(|r| r.owner)
}

/// Gets the heir of a store.
pub(crate) fn heir_of(id: StoreId) -> Option<Pid> {
    REGISTRY.stores.get(&id).and_then(|r| r.heir)
}

/// Gets the name of a store.
pub(crate) fn name_of(id: StoreId) -> Option<String> {
    REGISTRY.stores.get(&id).and_then(|r| r.name.clone())
}

/// Sets the heir of a store.
#[allow(dead_code)] // Useful API for dynamically setting heirs
pub fn set_heir(id: StoreId, heir: Option<Pid>) {
    if let Some(mut metadata) = REGISTRY.stores.get_mut(&id) {
        metadata.heir = heir;
    }
}

/// Cleans up stores owned by a process that has exited.
///
/// This is called from `handle_process_exit` in the runtime.
///
/// For each store owned by the exiting process:
/// - If an heir is set and alive, transfer ownership to the heir and notify
/// - Otherwise, mark the store as deleted and remove from registry
pub fn cleanup_owned_stores(pid: Pid, _reason: &ExitReason) {
    use super::StoreInherited;
    use crate::message::Message;

    // Get all stores owned by this process
    let store_ids: Vec<StoreId> = stores_owned_by(pid).into_iter().collect();

    for store_id in store_ids {
        // Check if there's an heir
        if let Some(heir_pid) = heir_of(store_id) {
            // Check if heir is alive
            if crate::alive(heir_pid) {
                // Get store name before transfer (for the notification)
                let store_name = name_of(store_id);

                // Transfer to heir
                transfer_ownership(store_id, pid, heir_pid);

                // Notify the heir about the inheritance
                if let Some(handle) = crate::try_handle() {
                    let msg = StoreInherited {
                        store_id,
                        old_owner: pid,
                        name: store_name,
                    };
                    // Best-effort send - ignore errors since the heir may have died
                    let _ = handle.registry().send_raw(heir_pid, msg.encode_local());
                }
                continue;
            }
        }

        // No heir or heir is dead - unregister the store
        unregister_store(store_id);
    }

    // Clean up the owner entry
    REGISTRY.by_owner.remove(&pid);
}

/// Statistics about the store registry.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Useful API for debugging/monitoring
pub struct StoreStats {
    /// Total number of stores.
    pub total_stores: usize,
    /// Number of named stores.
    pub named_stores: usize,
    /// Number of processes owning stores.
    pub owners: usize,
}

/// Returns statistics about the store registry.
#[allow(dead_code)] // Useful API for debugging/monitoring
pub fn stats() -> StoreStats {
    StoreStats {
        total_stores: REGISTRY.stores.len(),
        named_stores: REGISTRY.names.len(),
        owners: REGISTRY.by_owner.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let id = StoreId::new();
        let owner = Pid::new();

        register_store(id, owner, Some("test_store".into()), None);

        assert_eq!(whereis("test_store"), Some(id));
        assert_eq!(owner_of(id), Some(owner));
        assert!(stores_owned_by(owner).contains(&id));

        unregister_store(id);

        assert_eq!(whereis("test_store"), None);
        assert_eq!(owner_of(id), None);
    }

    #[test]
    fn test_transfer_ownership() {
        let id = StoreId::new();
        let owner1 = Pid::new();
        let owner2 = Pid::new();

        register_store(id, owner1, None, None);
        assert!(stores_owned_by(owner1).contains(&id));

        transfer_ownership(id, owner1, owner2);

        assert!(!stores_owned_by(owner1).contains(&id));
        assert!(stores_owned_by(owner2).contains(&id));
        assert_eq!(owner_of(id), Some(owner2));

        unregister_store(id);
    }

    #[test]
    fn test_heir() {
        let id = StoreId::new();
        let owner = Pid::new();
        let heir = Pid::new();

        register_store(id, owner, None, Some(heir));

        assert_eq!(heir_of(id), Some(heir));

        set_heir(id, None);
        assert_eq!(heir_of(id), None);

        unregister_store(id);
    }
}
