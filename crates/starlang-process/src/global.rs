//! Global runtime for Starlang.
//!
//! This module provides a global runtime that can be accessed from anywhere,
//! similar to how `tokio::spawn` works with tokio's global runtime.
//!
//! # Usage
//!
//! The global runtime is automatically initialized by the `#[starlang::main]` macro.
//! You can then use convenience functions like `starlang::spawn` anywhere in your code.
//!
//! ```ignore
//! use starlang::prelude::*;
//!
//! #[starlang::main]
//! async fn main() {
//!     // Spawn using the global runtime
//!     let pid = starlang::spawn(|| async move {
//!         println!("Hello from process {:?}", starlang::current_pid());
//!     });
//!
//!     // The handle is also available if needed
//!     let handle = starlang::handle();
//! }
//! ```

use crate::{Runtime, RuntimeHandle};
use starlang_core::Pid;
use std::future::Future;
use std::sync::OnceLock;

/// Global runtime instance.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Initializes the global runtime.
///
/// This is called automatically by the `#[starlang::main]` macro.
/// Calling this multiple times is safe - only the first call has any effect.
///
/// # Panics
///
/// This function does not panic. If the runtime is already initialized,
/// subsequent calls are no-ops.
pub fn init() {
    RUNTIME.get_or_init(Runtime::new);
}

/// Returns a handle to the global runtime.
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
/// Use `#[starlang::main]` or call `starlang::init()` first.
pub fn handle() -> RuntimeHandle {
    RUNTIME
        .get()
        .expect("Starlang runtime not initialized. Use #[starlang::main] or call starlang::init() first.")
        .handle()
}

/// Returns a handle to the global runtime, or `None` if not initialized.
pub fn try_handle() -> Option<RuntimeHandle> {
    RUNTIME.get().map(|r| r.handle())
}

/// Spawns a new process on the global runtime.
///
/// # Example
///
/// ```ignore
/// let pid = starlang::spawn(|| async move {
///     println!("Hello from {:?}", starlang::current_pid());
/// });
/// ```
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
pub fn spawn<F, Fut>(f: F) -> Pid
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle().spawn(f)
}

/// Spawns a new process linked to the given parent.
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
pub fn spawn_link<F, Fut>(parent: Pid, f: F) -> Pid
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle().spawn_link(parent, f)
}

/// Returns `true` if the process is alive.
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
pub fn alive(pid: Pid) -> bool {
    handle().alive(pid)
}

/// Looks up a process by registered name.
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
pub fn whereis(name: &str) -> Option<Pid> {
    handle().whereis(name)
}

/// Registers a name for a process.
///
/// Returns `true` if successful, `false` if the name is already taken.
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
pub fn register(name: impl Into<String>, pid: Pid) -> bool {
    handle().register(name.into(), pid)
}

/// Unregisters a name.
///
/// Returns the PID that was registered, or `None` if the name wasn't registered.
///
/// # Panics
///
/// Panics if the global runtime has not been initialized.
pub fn unregister(name: &str) -> Option<Pid> {
    handle().unregister(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn test_global_spawn() {
        init();

        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();

        let pid = spawn(move || async move {
            executed_clone.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(executed.load(Ordering::SeqCst));
        assert!(!alive(pid)); // Process finished
    }

    #[tokio::test]
    async fn test_global_register() {
        init();

        let pid = spawn(|| async move {
            // Keep alive for a bit
            let _ = starlang_runtime::recv_timeout(Duration::from_millis(200)).await;
        });

        assert!(register("test_process", pid));
        assert_eq!(whereis("test_process"), Some(pid));
        assert_eq!(unregister("test_process"), Some(pid));
        assert_eq!(whereis("test_process"), None);
    }

    #[tokio::test]
    async fn test_current_pid() {
        use std::sync::atomic::AtomicU64;

        init();

        let stored_pid = Arc::new(AtomicU64::new(0));
        let stored_pid_clone = stored_pid.clone();

        let spawned_pid = spawn(move || async move {
            // Get PID from task-local storage
            let current = starlang_runtime::current_pid();

            // Store for verification outside
            stored_pid_clone.store(current.id(), Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify the PID was stored and matches
        assert_eq!(stored_pid.load(Ordering::SeqCst), spawned_pid.id());
    }
}
