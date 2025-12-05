//! Process-owned concurrent key-value storage.
//!
//! The [`Store`] provides ETS-like functionality for Rust: typed, concurrent
//! key-value storage that is owned by a process and automatically cleaned up
//! when the owner exits.
//!
//! # Overview
//!
//! - **Typed**: `Store<K, V>` provides compile-time type safety
//! - **Process-owned**: Each store has an owner process; cleanup is automatic on owner exit
//! - **Concurrent**: Multiple processes can read/write (if access allows)
//! - **Named**: Stores can be registered by name for easy discovery
//!
//! # Example
//!
//! ```ignore
//! use ambitious::store::{Store, Access};
//!
//! // Create a public store (any process can access)
//! let users: Store<String, User> = Store::new();
//!
//! // Insert and retrieve values
//! users.insert("alice".into(), User { name: "Alice".into() });
//! let user = users.get(&"alice".into());
//!
//! // Store is automatically cleaned up when owner process exits
//! ```
//!
//! # Achieving Bag/DuplicateBag Patterns
//!
//! Unlike Erlang's ETS, this Store is typed and doesn't have special Bag types.
//! You can achieve similar functionality with typed values:
//!
//! ```ignore
//! use std::collections::HashSet;
//!
//! // Bag pattern - multiple unique values per key
//! let tags: Store<String, HashSet<String>> = Store::new();
//! tags.update("post_1", |existing| {
//!     let mut set = existing.unwrap_or_default();
//!     set.insert("rust".into());
//!     set
//! });
//!
//! // DuplicateBag pattern - multiple values including duplicates
//! let events: Store<String, Vec<Event>> = Store::new();
//! events.update("user_1", |existing| {
//!     let mut vec = existing.unwrap_or_default();
//!     vec.push(Event::Login);
//!     vec
//! });
//! ```

mod registry;

pub use registry::cleanup_owned_stores;

use crate::core::Pid;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Global counter for generating unique store IDs.
static STORE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique identifier for a store.
///
/// Store IDs are globally unique and atomically generated.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoreId(u64);

impl StoreId {
    /// Creates a new unique store ID.
    fn new() -> Self {
        Self(STORE_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Returns the raw value of this store ID.
    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

impl fmt::Debug for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StoreId({})", self.0)
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store#{}", self.0)
    }
}

/// Access control for a store.
///
/// Determines which processes can read from and write to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    /// Any process can read and write.
    #[default]
    Public,
    /// Only the owner process can read and write.
    Private,
}

/// Options for creating a store.
#[derive(Debug, Clone, Default)]
pub struct StoreOptions {
    /// Access control (default: Public).
    pub access: Access,
    /// Optional name for registration.
    pub name: Option<String>,
    /// Optional heir process that inherits the store on owner death.
    pub heir: Option<Pid>,
}

impl StoreOptions {
    /// Creates new store options with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the access control.
    pub fn access(mut self, access: Access) -> Self {
        self.access = access;
        self
    }

    /// Sets the store name for registration.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the heir process.
    pub fn heir(mut self, heir: Pid) -> Self {
        self.heir = Some(heir);
        self
    }
}

/// Error type for store operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The store has been deleted.
    StoreDeleted,
    /// Access denied (private store accessed by non-owner).
    AccessDenied,
    /// Name already registered.
    NameAlreadyRegistered,
    /// Store not found by name.
    NotFound,
    /// No process context available.
    NoProcessContext,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreDeleted => write!(f, "store has been deleted"),
            Self::AccessDenied => write!(f, "access denied"),
            Self::NameAlreadyRegistered => write!(f, "name already registered"),
            Self::NotFound => write!(f, "store not found"),
            Self::NoProcessContext => write!(f, "no process context available"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Internal store data that can be shared across clones.
struct StoreInner<K, V> {
    id: StoreId,
    owner: Pid,
    data: DashMap<K, V>,
    access: Access,
    #[allow(dead_code)] // Used in future named store lookup
    name: Option<String>,
    #[allow(dead_code)] // Used in heir transfer logic
    heir: Option<Pid>,
    deleted: std::sync::atomic::AtomicBool,
}

/// A typed, process-owned concurrent key-value store.
///
/// `Store<K, V>` provides concurrent access to key-value data with automatic
/// cleanup when the owning process terminates.
///
/// # Type Parameters
///
/// - `K`: Key type, must be `Eq + Hash + Clone + Send + Sync`
/// - `V`: Value type, must be `Clone + Send + Sync`
///
/// # Thread Safety
///
/// Store is fully thread-safe and can be shared across threads and processes.
/// The underlying data structure uses lock-free concurrent hashmaps.
///
/// # Example
///
/// ```ignore
/// use ambitious::store::Store;
///
/// let store: Store<String, i32> = Store::new();
/// store.insert("count".into(), 0);
///
/// // Atomic update
/// store.update("count", |v| v.map(|n| n + 1).unwrap_or(1));
/// ```
pub struct Store<K, V> {
    inner: Arc<StoreInner<K, V>>,
}

impl<K, V> Clone for Store<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> Default for Store<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> Store<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a new store owned by the current process.
    ///
    /// # Panics
    ///
    /// Panics if called outside of a process context.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use ambitious::store::Store;
    ///
    /// let store: Store<String, User> = Store::new();
    /// ```
    pub fn new() -> Self {
        Self::with_options(StoreOptions::default())
    }

    /// Creates a new store with the given options.
    ///
    /// # Panics
    ///
    /// Panics if called outside of a process context.
    pub fn with_options(opts: StoreOptions) -> Self {
        let owner = crate::try_current_pid().expect("Store::new() must be called from a process");
        Self::new_with_owner(owner, opts)
    }

    /// Creates a new store with an explicit owner (for internal use).
    pub(crate) fn new_with_owner(owner: Pid, opts: StoreOptions) -> Self {
        let id = StoreId::new();

        let inner = Arc::new(StoreInner {
            id,
            owner,
            data: DashMap::new(),
            access: opts.access,
            name: opts.name.clone(),
            heir: opts.heir,
            deleted: std::sync::atomic::AtomicBool::new(false),
        });

        let store = Self { inner };

        // Register in global registry
        registry::register_store(id, owner, opts.name, opts.heir);

        store
    }

    /// Returns the unique ID of this store.
    pub fn id(&self) -> StoreId {
        self.inner.id
    }

    /// Returns the PID of the owner process.
    pub fn owner(&self) -> Pid {
        self.inner.owner
    }

    /// Returns the access control setting.
    pub fn access(&self) -> Access {
        self.inner.access
    }

    /// Returns `true` if this store has been deleted.
    pub fn is_deleted(&self) -> bool {
        self.inner.deleted.load(Ordering::Acquire)
    }

    /// Checks if the current process can access this store.
    fn check_access(&self) -> Result<(), StoreError> {
        if self.is_deleted() {
            return Err(StoreError::StoreDeleted);
        }

        match self.inner.access {
            Access::Public => Ok(()),
            Access::Private => {
                let caller = crate::try_current_pid().ok_or(StoreError::NoProcessContext)?;
                if caller == self.inner.owner {
                    Ok(())
                } else {
                    Err(StoreError::AccessDenied)
                }
            }
        }
    }

    // =========================================================================
    // Basic Operations
    // =========================================================================

    /// Inserts a key-value pair into the store.
    ///
    /// Returns the previous value if the key was already present.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn insert(&self, key: K, value: V) -> Result<Option<V>, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.insert(key, value))
    }

    /// Gets a value by key.
    ///
    /// Returns `None` if the key is not present.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn get(&self, key: &K) -> Result<Option<V>, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.get(key).map(|r| r.value().clone()))
    }

    /// Removes a key from the store.
    ///
    /// Returns the removed value if the key was present.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn remove(&self, key: &K) -> Result<Option<V>, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.remove(key).map(|(_, v)| v))
    }

    /// Returns `true` if the store contains the given key.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn contains_key(&self, key: &K) -> Result<bool, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.contains_key(key))
    }

    /// Atomically updates a value in the store.
    ///
    /// The update function receives the current value (if any) and returns
    /// the new value to store.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn update<F>(&self, key: K, f: F) -> Result<V, StoreError>
    where
        F: FnOnce(Option<V>) -> V,
    {
        self.check_access()?;

        let new_value = f(self.inner.data.get(&key).map(|r| r.value().clone()));
        self.inner.data.insert(key, new_value.clone());
        Ok(new_value)
    }

    /// Gets a value or inserts a default if not present.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn get_or_insert(&self, key: K, default: V) -> Result<V, StoreError> {
        self.check_access()?;
        Ok(self
            .inner
            .data
            .entry(key)
            .or_insert(default)
            .value()
            .clone())
    }

    /// Gets a value or inserts a default computed by a function.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn get_or_insert_with<F>(&self, key: K, f: F) -> Result<V, StoreError>
    where
        F: FnOnce() -> V,
    {
        self.check_access()?;
        Ok(self.inner.data.entry(key).or_insert_with(f).value().clone())
    }

    // =========================================================================
    // Bulk Operations
    // =========================================================================

    /// Inserts multiple key-value pairs.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn insert_many(&self, items: impl IntoIterator<Item = (K, V)>) -> Result<(), StoreError> {
        self.check_access()?;
        for (k, v) in items {
            self.inner.data.insert(k, v);
        }
        Ok(())
    }

    /// Returns all keys in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn keys(&self) -> Result<Vec<K>, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.iter().map(|r| r.key().clone()).collect())
    }

    /// Returns all values in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn values(&self) -> Result<Vec<V>, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.iter().map(|r| r.value().clone()).collect())
    }

    /// Returns all key-value pairs in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn iter(&self) -> Result<Vec<(K, V)>, StoreError> {
        self.check_access()?;
        Ok(self
            .inner
            .data
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect())
    }

    /// Returns the number of entries in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn len(&self) -> Result<usize, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.len())
    }

    /// Returns `true` if the store is empty.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn is_empty(&self) -> Result<bool, StoreError> {
        self.check_access()?;
        Ok(self.inner.data.is_empty())
    }

    /// Removes all entries from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn clear(&self) -> Result<(), StoreError> {
        self.check_access()?;
        self.inner.data.clear();
        Ok(())
    }

    /// Retains only the entries that satisfy the predicate.
    ///
    /// # Errors
    ///
    /// Returns an error if access is denied or the store is deleted.
    pub fn retain<F>(&self, f: F) -> Result<(), StoreError>
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        self.check_access()?;
        self.inner.data.retain(f);
        Ok(())
    }

    // =========================================================================
    // Ownership Operations
    // =========================================================================

    /// Transfers ownership of this store to another process.
    ///
    /// Only the current owner can transfer ownership.
    ///
    /// # Errors
    ///
    /// Returns an error if the caller is not the owner.
    pub fn give_away(&self, new_owner: Pid) -> Result<(), StoreError> {
        let caller = crate::try_current_pid().ok_or(StoreError::NoProcessContext)?;

        if caller != self.inner.owner {
            return Err(StoreError::AccessDenied);
        }

        registry::transfer_ownership(self.inner.id, self.inner.owner, new_owner);

        // Note: We can't actually update inner.owner since it's behind Arc.
        // The registry is the source of truth for ownership.
        Ok(())
    }

    /// Marks this store as deleted.
    ///
    /// Called internally during cleanup.
    #[allow(dead_code)] // Will be used when store cleanup marks stores deleted
    pub(crate) fn mark_deleted(&self) {
        self.inner.deleted.store(true, Ordering::Release);
    }
}

impl<K, V> fmt::Debug for Store<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("id", &self.inner.id)
            .field("owner", &self.inner.owner)
            .field("access", &self.inner.access)
            .field("len", &self.inner.data.len())
            .finish()
    }
}

// ============================================================================
// OrderedStore - BTreeMap-backed store with sorted keys
// ============================================================================

/// Internal ordered store data that can be shared across clones.
struct OrderedStoreInner<K, V> {
    id: StoreId,
    owner: Pid,
    data: RwLock<BTreeMap<K, V>>,
    access: Access,
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    heir: Option<Pid>,
    deleted: std::sync::atomic::AtomicBool,
}

/// A typed, process-owned concurrent key-value store with ordered keys.
///
/// `OrderedStore<K, V>` is similar to [`Store`] but maintains keys in sorted
/// order, enabling efficient range queries and ordered iteration.
///
/// # Type Parameters
///
/// - `K`: Key type, must be `Ord + Clone + Send + Sync`
/// - `V`: Value type, must be `Clone + Send + Sync`
///
/// # Thread Safety
///
/// OrderedStore is thread-safe using read-write locks. Multiple readers can
/// access concurrently, but writes require exclusive access.
///
/// # Example
///
/// ```ignore
/// use ambitious::store::OrderedStore;
///
/// let scores: OrderedStore<i32, String> = OrderedStore::new();
/// scores.insert(100, "Alice".into());
/// scores.insert(85, "Bob".into());
/// scores.insert(92, "Charlie".into());
///
/// // Get entries in sorted order
/// let top_scores = scores.range(90.., 10)?;  // All scores >= 90
/// ```
pub struct OrderedStore<K, V> {
    inner: Arc<OrderedStoreInner<K, V>>,
}

impl<K, V> Clone for OrderedStore<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> Default for OrderedStore<K, V>
where
    K: Ord + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> OrderedStore<K, V>
where
    K: Ord + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a new ordered store owned by the current process.
    ///
    /// # Panics
    ///
    /// Panics if called outside of a process context.
    pub fn new() -> Self {
        Self::with_options(StoreOptions::default())
    }

    /// Creates a new ordered store with the given options.
    ///
    /// # Panics
    ///
    /// Panics if called outside of a process context.
    pub fn with_options(opts: StoreOptions) -> Self {
        let owner =
            crate::try_current_pid().expect("OrderedStore::new() must be called from a process");
        Self::new_with_owner(owner, opts)
    }

    /// Creates a new ordered store with an explicit owner (for internal use).
    pub(crate) fn new_with_owner(owner: Pid, opts: StoreOptions) -> Self {
        let id = StoreId::new();

        let inner = Arc::new(OrderedStoreInner {
            id,
            owner,
            data: RwLock::new(BTreeMap::new()),
            access: opts.access,
            name: opts.name.clone(),
            heir: opts.heir,
            deleted: std::sync::atomic::AtomicBool::new(false),
        });

        let store = Self { inner };

        // Register in global registry
        registry::register_store(id, owner, opts.name, opts.heir);

        store
    }

    /// Returns the unique ID of this store.
    pub fn id(&self) -> StoreId {
        self.inner.id
    }

    /// Returns the PID of the owner process.
    pub fn owner(&self) -> Pid {
        self.inner.owner
    }

    /// Returns the access control setting.
    pub fn access(&self) -> Access {
        self.inner.access
    }

    /// Returns `true` if this store has been deleted.
    pub fn is_deleted(&self) -> bool {
        self.inner.deleted.load(Ordering::Acquire)
    }

    /// Checks if the current process can access this store.
    fn check_access(&self) -> Result<(), StoreError> {
        if self.is_deleted() {
            return Err(StoreError::StoreDeleted);
        }

        match self.inner.access {
            Access::Public => Ok(()),
            Access::Private => {
                let caller = crate::try_current_pid().ok_or(StoreError::NoProcessContext)?;
                if caller == self.inner.owner {
                    Ok(())
                } else {
                    Err(StoreError::AccessDenied)
                }
            }
        }
    }

    // =========================================================================
    // Basic Operations
    // =========================================================================

    /// Inserts a key-value pair into the store.
    ///
    /// Returns the previous value if the key was already present.
    pub fn insert(&self, key: K, value: V) -> Result<Option<V>, StoreError> {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        Ok(data.insert(key, value))
    }

    /// Gets a value by key.
    pub fn get(&self, key: &K) -> Result<Option<V>, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.get(key).cloned())
    }

    /// Removes a key from the store.
    pub fn remove(&self, key: &K) -> Result<Option<V>, StoreError> {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        Ok(data.remove(key))
    }

    /// Returns `true` if the store contains the given key.
    pub fn contains_key(&self, key: &K) -> Result<bool, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.contains_key(key))
    }

    /// Atomically updates a value in the store.
    pub fn update<F>(&self, key: K, f: F) -> Result<V, StoreError>
    where
        F: FnOnce(Option<V>) -> V,
    {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        let new_value = f(data.get(&key).cloned());
        data.insert(key, new_value.clone());
        Ok(new_value)
    }

    // =========================================================================
    // Bulk Operations
    // =========================================================================

    /// Inserts multiple key-value pairs.
    pub fn insert_many(&self, items: impl IntoIterator<Item = (K, V)>) -> Result<(), StoreError> {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        for (k, v) in items {
            data.insert(k, v);
        }
        Ok(())
    }

    /// Returns all keys in sorted order.
    pub fn keys(&self) -> Result<Vec<K>, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.keys().cloned().collect())
    }

    /// Returns all values in key-sorted order.
    pub fn values(&self) -> Result<Vec<V>, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.values().cloned().collect())
    }

    /// Returns all key-value pairs in sorted order.
    pub fn iter(&self) -> Result<Vec<(K, V)>, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// Returns the number of entries in the store.
    pub fn len(&self) -> Result<usize, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.len())
    }

    /// Returns `true` if the store is empty.
    pub fn is_empty(&self) -> Result<bool, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.is_empty())
    }

    /// Removes all entries from the store.
    pub fn clear(&self) -> Result<(), StoreError> {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        data.clear();
        Ok(())
    }

    /// Retains only the entries that satisfy the predicate.
    pub fn retain<F>(&self, mut f: F) -> Result<(), StoreError>
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        data.retain(|k, v| f(k, v));
        Ok(())
    }

    // =========================================================================
    // Ordered Operations (unique to OrderedStore)
    // =========================================================================

    /// Returns the first (smallest) key-value pair.
    pub fn first(&self) -> Result<Option<(K, V)>, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.first_key_value().map(|(k, v)| (k.clone(), v.clone())))
    }

    /// Returns the last (largest) key-value pair.
    pub fn last(&self) -> Result<Option<(K, V)>, StoreError> {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.last_key_value().map(|(k, v)| (k.clone(), v.clone())))
    }

    /// Removes and returns the first (smallest) key-value pair.
    pub fn pop_first(&self) -> Result<Option<(K, V)>, StoreError> {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        Ok(data.pop_first())
    }

    /// Removes and returns the last (largest) key-value pair.
    pub fn pop_last(&self) -> Result<Option<(K, V)>, StoreError> {
        self.check_access()?;
        let mut data = self.inner.data.write().unwrap();
        Ok(data.pop_last())
    }

    /// Returns key-value pairs in a range, up to a limit.
    ///
    /// # Arguments
    ///
    /// * `range` - The range of keys to include
    /// * `limit` - Maximum number of entries to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get up to 10 entries with keys >= 50
    /// let entries = store.range(50.., 10)?;
    ///
    /// // Get up to 5 entries with keys in [10, 20]
    /// let entries = store.range(10..=20, 5)?;
    /// ```
    pub fn range<R>(&self, range: R, limit: usize) -> Result<Vec<(K, V)>, StoreError>
    where
        R: std::ops::RangeBounds<K>,
    {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data
            .range(range)
            .take(limit)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    /// Returns key-value pairs in a range in reverse order, up to a limit.
    pub fn range_rev<R>(&self, range: R, limit: usize) -> Result<Vec<(K, V)>, StoreError>
    where
        R: std::ops::RangeBounds<K>,
    {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();

        // Convert bounds for rev iteration
        let start = range.start_bound();
        let end = range.end_bound();

        Ok(data
            .range((
                match start {
                    Bound::Included(k) => Bound::Included(k.clone()),
                    Bound::Excluded(k) => Bound::Excluded(k.clone()),
                    Bound::Unbounded => Bound::Unbounded,
                },
                match end {
                    Bound::Included(k) => Bound::Included(k.clone()),
                    Bound::Excluded(k) => Bound::Excluded(k.clone()),
                    Bound::Unbounded => Bound::Unbounded,
                },
            ))
            .rev()
            .take(limit)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    /// Returns the number of entries in a range.
    pub fn range_count<R>(&self, range: R) -> Result<usize, StoreError>
    where
        R: std::ops::RangeBounds<K>,
    {
        self.check_access()?;
        let data = self.inner.data.read().unwrap();
        Ok(data.range(range).count())
    }

    // =========================================================================
    // Ownership Operations
    // =========================================================================

    /// Transfers ownership of this store to another process.
    pub fn give_away(&self, new_owner: Pid) -> Result<(), StoreError> {
        let caller = crate::try_current_pid().ok_or(StoreError::NoProcessContext)?;

        if caller != self.inner.owner {
            return Err(StoreError::AccessDenied);
        }

        registry::transfer_ownership(self.inner.id, self.inner.owner, new_owner);
        Ok(())
    }

    /// Marks this store as deleted.
    #[allow(dead_code)]
    pub(crate) fn mark_deleted(&self) {
        self.inner.deleted.store(true, Ordering::Release);
    }
}

impl<K, V> fmt::Debug for OrderedStore<K, V>
where
    K: Ord + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.inner.data.read().map(|d| d.len()).unwrap_or(0);
        f.debug_struct("OrderedStore")
            .field("id", &self.inner.id)
            .field("owner", &self.inner.owner)
            .field("access", &self.inner.access)
            .field("len", &len)
            .finish()
    }
}

// ============================================================================
// Global Store Lookup
// ============================================================================

/// Looks up a store by name.
///
/// Returns the store ID if found.
pub fn whereis(name: &str) -> Option<StoreId> {
    registry::whereis(name)
}

/// Returns all store IDs owned by a process.
pub fn stores_owned_by(pid: Pid) -> HashSet<StoreId> {
    registry::stores_owned_by(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a store outside of process context for testing
    fn create_test_store<K, V>() -> Store<K, V>
    where
        K: Eq + Hash + Clone + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        let pid = Pid::new();
        Store::new_with_owner(pid, StoreOptions::default())
    }

    #[test]
    fn test_store_id_uniqueness() {
        let id1 = StoreId::new();
        let id2 = StoreId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_store_basic_operations() {
        let store: Store<String, i32> = create_test_store();

        // Insert
        assert_eq!(store.insert("a".into(), 1).unwrap(), None);
        assert_eq!(store.insert("a".into(), 2).unwrap(), Some(1));

        // Get
        assert_eq!(store.get(&"a".into()).unwrap(), Some(2));
        assert_eq!(store.get(&"b".into()).unwrap(), None);

        // Contains
        assert!(store.contains_key(&"a".into()).unwrap());
        assert!(!store.contains_key(&"b".into()).unwrap());

        // Remove
        assert_eq!(store.remove(&"a".into()).unwrap(), Some(2));
        assert_eq!(store.remove(&"a".into()).unwrap(), None);
    }

    #[test]
    fn test_store_update() {
        let store: Store<String, i32> = create_test_store();

        // Update non-existent key
        let val = store
            .update("count".into(), |v| v.unwrap_or(0) + 1)
            .unwrap();
        assert_eq!(val, 1);

        // Update existing key
        let val = store
            .update("count".into(), |v| v.unwrap_or(0) + 1)
            .unwrap();
        assert_eq!(val, 2);

        assert_eq!(store.get(&"count".into()).unwrap(), Some(2));
    }

    #[test]
    fn test_store_bulk_operations() {
        let store: Store<String, i32> = create_test_store();

        store
            .insert_many([("a".into(), 1), ("b".into(), 2), ("c".into(), 3)])
            .unwrap();

        assert_eq!(store.len().unwrap(), 3);
        assert!(!store.is_empty().unwrap());

        let keys = store.keys().unwrap();
        assert_eq!(keys.len(), 3);

        let values = store.values().unwrap();
        assert_eq!(values.len(), 3);

        store.clear().unwrap();
        assert!(store.is_empty().unwrap());
    }

    #[test]
    fn test_store_retain() {
        let store: Store<String, i32> = create_test_store();

        store
            .insert_many([("a".into(), 1), ("b".into(), 2), ("c".into(), 3)])
            .unwrap();

        store.retain(|_, v| *v > 1).unwrap();

        assert_eq!(store.len().unwrap(), 2);
        assert!(!store.contains_key(&"a".into()).unwrap());
        assert!(store.contains_key(&"b".into()).unwrap());
        assert!(store.contains_key(&"c".into()).unwrap());
    }

    #[test]
    fn test_store_clone() {
        let store1: Store<String, i32> = create_test_store();
        store1.insert("key".into(), 42).unwrap();

        let store2 = store1.clone();
        assert_eq!(store2.get(&"key".into()).unwrap(), Some(42));

        // Modifications through one clone are visible through the other
        store2.insert("key".into(), 100).unwrap();
        assert_eq!(store1.get(&"key".into()).unwrap(), Some(100));
    }

    #[test]
    fn test_store_deleted() {
        let store: Store<String, i32> = create_test_store();
        store.insert("key".into(), 1).unwrap();

        store.mark_deleted();

        assert!(store.is_deleted());
        assert!(matches!(
            store.get(&"key".into()),
            Err(StoreError::StoreDeleted)
        ));
        assert!(matches!(
            store.insert("key".into(), 2),
            Err(StoreError::StoreDeleted)
        ));
    }

    #[test]
    fn test_store_options() {
        let opts = StoreOptions::new().access(Access::Private).name("my_store");

        assert_eq!(opts.access, Access::Private);
        assert_eq!(opts.name, Some("my_store".into()));
    }

    // =========================================================================
    // OrderedStore Tests
    // =========================================================================

    fn create_test_ordered_store<K, V>() -> OrderedStore<K, V>
    where
        K: Ord + Clone + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        let pid = Pid::new();
        OrderedStore::new_with_owner(pid, StoreOptions::default())
    }

    #[test]
    fn test_ordered_store_basic_operations() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        // Insert
        assert_eq!(store.insert(1, "one".into()).unwrap(), None);
        assert_eq!(store.insert(1, "ONE".into()).unwrap(), Some("one".into()));

        // Get
        assert_eq!(store.get(&1).unwrap(), Some("ONE".into()));
        assert_eq!(store.get(&2).unwrap(), None);

        // Contains
        assert!(store.contains_key(&1).unwrap());
        assert!(!store.contains_key(&2).unwrap());

        // Remove
        assert_eq!(store.remove(&1).unwrap(), Some("ONE".into()));
        assert_eq!(store.remove(&1).unwrap(), None);
    }

    #[test]
    fn test_ordered_store_sorted_keys() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        // Insert in random order
        store.insert(30, "thirty".into()).unwrap();
        store.insert(10, "ten".into()).unwrap();
        store.insert(20, "twenty".into()).unwrap();

        // Keys should be in sorted order
        let keys = store.keys().unwrap();
        assert_eq!(keys, vec![10, 20, 30]);

        // Values should be in key-sorted order
        let values = store.values().unwrap();
        assert_eq!(values, vec!["ten", "twenty", "thirty"]);

        // Iter should be in sorted order
        let entries = store.iter().unwrap();
        assert_eq!(
            entries,
            vec![
                (10, "ten".into()),
                (20, "twenty".into()),
                (30, "thirty".into())
            ]
        );
    }

    #[test]
    fn test_ordered_store_first_last() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        // Empty store
        assert_eq!(store.first().unwrap(), None);
        assert_eq!(store.last().unwrap(), None);

        store.insert(20, "twenty".into()).unwrap();
        store.insert(10, "ten".into()).unwrap();
        store.insert(30, "thirty".into()).unwrap();

        assert_eq!(store.first().unwrap(), Some((10, "ten".into())));
        assert_eq!(store.last().unwrap(), Some((30, "thirty".into())));
    }

    #[test]
    fn test_ordered_store_pop_first_last() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        store.insert(10, "ten".into()).unwrap();
        store.insert(20, "twenty".into()).unwrap();
        store.insert(30, "thirty".into()).unwrap();

        assert_eq!(store.pop_first().unwrap(), Some((10, "ten".into())));
        assert_eq!(store.len().unwrap(), 2);

        assert_eq!(store.pop_last().unwrap(), Some((30, "thirty".into())));
        assert_eq!(store.len().unwrap(), 1);

        assert_eq!(store.first().unwrap(), Some((20, "twenty".into())));
    }

    #[test]
    fn test_ordered_store_range() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        for i in 1..=10 {
            store.insert(i * 10, format!("val{}", i)).unwrap();
        }

        // Range with both bounds
        let entries = store.range(30..=70, 100).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].0, 30);
        assert_eq!(entries[4].0, 70);

        // Range with limit
        let entries = store.range(30..=70, 2).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, 30);
        assert_eq!(entries[1].0, 40);

        // Range from start
        let entries = store.range(..=30, 100).unwrap();
        assert_eq!(entries.len(), 3);

        // Range to end
        let entries = store.range(80.., 100).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_ordered_store_range_rev() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        for i in 1..=5 {
            store.insert(i * 10, format!("val{}", i)).unwrap();
        }

        let entries = store.range_rev(20..=40, 100).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, 40); // Reversed order
        assert_eq!(entries[1].0, 30);
        assert_eq!(entries[2].0, 20);
    }

    #[test]
    fn test_ordered_store_range_count() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        for i in 1..=10 {
            store.insert(i * 10, format!("val{}", i)).unwrap();
        }

        assert_eq!(store.range_count(30..=70).unwrap(), 5);
        assert_eq!(store.range_count(..=30).unwrap(), 3);
        assert_eq!(store.range_count(80..).unwrap(), 3);
        assert_eq!(store.range_count(..).unwrap(), 10);
    }

    #[test]
    fn test_ordered_store_clone() {
        let store1: OrderedStore<i32, String> = create_test_ordered_store();
        store1.insert(1, "one".into()).unwrap();

        let store2 = store1.clone();
        assert_eq!(store2.get(&1).unwrap(), Some("one".into()));

        // Modifications through one clone are visible through the other
        store2.insert(1, "ONE".into()).unwrap();
        assert_eq!(store1.get(&1).unwrap(), Some("ONE".into()));
    }

    #[test]
    fn test_ordered_store_retain() {
        let store: OrderedStore<i32, String> = create_test_ordered_store();

        store.insert(1, "one".into()).unwrap();
        store.insert(2, "two".into()).unwrap();
        store.insert(3, "three".into()).unwrap();

        store.retain(|k, _| *k > 1).unwrap();

        assert_eq!(store.len().unwrap(), 2);
        assert!(!store.contains_key(&1).unwrap());
        assert!(store.contains_key(&2).unwrap());
        assert!(store.contains_key(&3).unwrap());
    }
}
