//! Process spawning functions.
//!
//! These functions provide convenient ways to spawn processes using
//! a global or thread-local runtime.

use super::RuntimeHandle;
use crate::core::{Pid, Ref};
use std::future::Future;

/// Type alias for process functions.
pub type ProcessFn<Fut> = Box<dyn FnOnce() -> Fut + Send + 'static>;

/// Spawns a new process using the provided runtime handle.
///
/// # Example
///
/// ```ignore
/// let pid = spawn(&handle, || async move {
///     println!("Hello from process {:?}", ambitious::current_pid());
/// });
/// ```
pub fn spawn<F, Fut>(handle: &RuntimeHandle, f: F) -> Pid
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle.spawn(f)
}

/// Spawns a new process linked to the parent.
///
/// # Example
///
/// ```ignore
/// let child = spawn_link(&handle, parent_pid, || async move {
///     println!("Linked child process");
/// });
/// ```
pub fn spawn_link<F, Fut>(handle: &RuntimeHandle, parent: Pid, f: F) -> Pid
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle.spawn_link(parent, f)
}

/// Spawns a new process and monitors it.
///
/// Returns the PID and monitor reference.
///
/// # Example
///
/// ```ignore
/// let (child, ref) = spawn_monitor(&handle, monitor_pid, || async move {
///     println!("Monitored child process");
/// });
/// ```
pub fn spawn_monitor<F, Fut>(handle: &RuntimeHandle, monitor: Pid, f: F) -> (Pid, Ref)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle.spawn_monitor(monitor, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Runtime;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_spawn_function() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let _pid = spawn(&handle, move || async move {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(50)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_spawn_multiple() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..10 {
            let counter_clone = counter.clone();
            spawn(&handle, move || async move {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });
        }

        sleep(Duration::from_millis(100)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 10);
    }
}
