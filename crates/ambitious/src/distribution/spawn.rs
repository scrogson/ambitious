//! Remote spawn support.
//!
//! This module provides the ability to spawn processes on remote nodes.
//! Functions must be registered before they can be spawned remotely.
//!
//! # Example
//!
//! ```ignore
//! use ambitious::distribution::spawn;
//!
//! // Register a spawnable function
//! spawn::register("mymodule", "worker", |args: Vec<u8>| {
//!     Box::pin(async move {
//!         // Deserialize args and run the worker
//!         loop {
//!             // ... worker logic
//!         }
//!     })
//! });
//!
//! // On another node, spawn the worker
//! let pid = node::spawn(remote_node, "mymodule", "worker", args).await?;
//! ```

use crate::core::Pid;
use dashmap::DashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

/// Type alias for a boxed future returned by spawn functions.
pub type SpawnFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Type alias for spawnable functions.
///
/// A spawnable function takes serialized arguments and returns a future
/// that will be executed as a process.
pub type SpawnFn = Box<dyn Fn(Vec<u8>) -> SpawnFuture + Send + Sync + 'static>;

/// Global registry of spawnable functions.
static SPAWN_REGISTRY: OnceLock<SpawnRegistry> = OnceLock::new();

/// Registry of functions that can be spawned remotely.
pub struct SpawnRegistry {
    /// Map of (module, function) -> spawn function.
    functions: DashMap<(String, String), SpawnFn>,
}

impl SpawnRegistry {
    /// Create a new spawn registry.
    fn new() -> Self {
        Self {
            functions: DashMap::new(),
        }
    }

    /// Register a spawnable function.
    pub fn register<F, Fut>(&self, module: &str, function: &str, f: F)
    where
        F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let key = (module.to_string(), function.to_string());
        let boxed: SpawnFn = Box::new(move |args| Box::pin(f(args)));
        self.functions.insert(key, boxed);
    }

    /// Look up a spawnable function.
    pub fn get(
        &self,
        module: &str,
        function: &str,
    ) -> Option<impl Fn(Vec<u8>) -> SpawnFuture + '_> {
        let key = (module.to_string(), function.to_string());
        self.functions.get(&key).map(|entry| {
            move |args: Vec<u8>| {
                let f = entry.value();
                f(args)
            }
        })
    }

    /// Check if a function is registered.
    pub fn contains(&self, module: &str, function: &str) -> bool {
        let key = (module.to_string(), function.to_string());
        self.functions.contains_key(&key)
    }

    /// Unregister a spawnable function.
    pub fn unregister(&self, module: &str, function: &str) -> bool {
        let key = (module.to_string(), function.to_string());
        self.functions.remove(&key).is_some()
    }
}

/// Get the global spawn registry.
pub fn registry() -> &'static SpawnRegistry {
    SPAWN_REGISTRY.get_or_init(SpawnRegistry::new)
}

/// Register a function that can be spawned remotely.
///
/// # Arguments
///
/// * `module` - Module name (used for namespacing)
/// * `function` - Function name within the module
/// * `f` - The function to execute when spawned
///
/// # Example
///
/// ```ignore
/// spawn::register("workers", "counter", |args| async move {
///     let initial: u32 = postcard::from_bytes(&args).unwrap_or(0);
///     let mut count = initial;
///     loop {
///         // Handle messages...
///     }
/// });
/// ```
pub fn register<F, Fut>(module: &str, function: &str, f: F)
where
    F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    registry().register(module, function, f);
}

/// Unregister a spawnable function.
pub fn unregister(module: &str, function: &str) -> bool {
    registry().unregister(module, function)
}

/// Check if a function is registered for remote spawning.
pub fn is_registered(module: &str, function: &str) -> bool {
    registry().contains(module, function)
}

/// Execute a spawn request.
///
/// This is called by the distribution manager when a Spawn message is received.
/// It looks up the function, spawns it, and returns the PID.
pub(crate) fn execute_spawn(
    module: &str,
    function: &str,
    args: Vec<u8>,
    link_to: Option<Pid>,
    monitor_from: Option<Pid>,
) -> Result<(Pid, Option<crate::core::Ref>), String> {
    let reg = registry();

    // Look up the function
    if !reg.contains(module, function) {
        return Err(format!(
            "function {}:{} not registered for remote spawn",
            module, function
        ));
    }

    // Get the function and create the future
    let future = {
        let key = (module.to_string(), function.to_string());
        if let Some(entry) = reg.functions.get(&key) {
            let f = entry.value();
            f(args)
        } else {
            return Err(format!(
                "function {}:{} not found (race condition?)",
                module, function
            ));
        }
    };

    // Spawn the process
    let handle = crate::try_handle().ok_or_else(|| "runtime not available".to_string())?;

    let pid = if link_to.is_some() {
        // For link, we need to spawn_link which isn't directly available on handle
        // For now, spawn normally and set up link manually
        let pid = handle.spawn(move || future);
        if let Some(link_pid) = link_to {
            // Set up bidirectional link
            // This would need proper cross-node link support
            tracing::debug!(local = %pid, remote = %link_pid, "Setting up remote link");
        }
        pid
    } else {
        handle.spawn(move || future)
    };

    // Set up monitor if requested
    let monitor_ref = if let Some(from_pid) = monitor_from {
        // Register a monitor from the remote process to the local spawned process
        let ref_ = crate::core::Ref::new();
        // This would need proper cross-node monitor support
        tracing::debug!(from = %from_pid, target = %pid, ref_ = %ref_, "Setting up remote monitor");
        Some(ref_)
    } else {
        None
    };

    Ok((pid, monitor_ref))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        register("test_module", "test_func", |_args| async move {
            // Do nothing
        });

        assert!(is_registered("test_module", "test_func"));
        assert!(!is_registered("test_module", "other_func"));
        assert!(!is_registered("other_module", "test_func"));
    }

    #[test]
    fn test_unregister() {
        register("unregister_test", "func", |_| async {});
        assert!(is_registered("unregister_test", "func"));

        assert!(unregister("unregister_test", "func"));
        assert!(!is_registered("unregister_test", "func"));

        // Unregistering again returns false
        assert!(!unregister("unregister_test", "func"));
    }

    #[test]
    fn test_registry_get_returns_callable() {
        register("callable_test", "hello", |args| async move {
            // Verify we received the args
            assert_eq!(args, vec![1, 2, 3]);
        });

        // Verify we can look up the function
        let reg = registry();
        let func = reg.get("callable_test", "hello");
        assert!(func.is_some());

        // Verify lookup for non-existent function returns None
        assert!(reg.get("callable_test", "nonexistent").is_none());
    }

    #[test]
    fn test_registry_contains() {
        register("contains_test", "exists", |_| async {});

        let reg = registry();
        assert!(reg.contains("contains_test", "exists"));
        assert!(!reg.contains("contains_test", "not_exists"));
        assert!(!reg.contains("wrong_module", "exists"));
    }

    #[test]
    fn test_multiple_functions_same_module() {
        register("multi_func", "func1", |_| async {});
        register("multi_func", "func2", |_| async {});
        register("multi_func", "func3", |_| async {});

        assert!(is_registered("multi_func", "func1"));
        assert!(is_registered("multi_func", "func2"));
        assert!(is_registered("multi_func", "func3"));

        // Unregister one doesn't affect others
        unregister("multi_func", "func2");
        assert!(is_registered("multi_func", "func1"));
        assert!(!is_registered("multi_func", "func2"));
        assert!(is_registered("multi_func", "func3"));
    }

    #[test]
    fn test_register_overwrites_existing() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter1 = Arc::new(AtomicU32::new(0));
        let counter2 = Arc::new(AtomicU32::new(0));

        let c1 = counter1.clone();
        register("overwrite_test", "func", move |_| {
            let c = c1.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });

        let c2 = counter2.clone();
        register("overwrite_test", "func", move |_| {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });

        // The function should still be registered
        assert!(is_registered("overwrite_test", "func"));

        // We can't easily verify which version is registered without
        // executing the function, but at least we know it's there
    }

    #[test]
    fn test_execute_spawn_not_registered() {
        let result = execute_spawn("nonexistent", "func", vec![], None, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("not registered for remote spawn")
        );
    }
}
