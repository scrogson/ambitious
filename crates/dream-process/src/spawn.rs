//! Process spawning functions.
//!
//! These functions provide convenient ways to spawn processes using
//! a global or thread-local runtime.

use crate::runtime::RuntimeHandle;
use dream_core::{Pid, Ref};
use dream_runtime::Context;
use std::future::Future;

/// Type alias for process functions.
pub type ProcessFn<Fut> = Box<dyn FnOnce(Context) -> Fut + Send + 'static>;

/// Spawns a new process using the provided runtime handle.
///
/// # Example
///
/// ```ignore
/// let pid = spawn(&handle, |ctx| async move {
///     println!("Hello from process {:?}", ctx.pid());
/// });
/// ```
pub fn spawn<F, Fut>(handle: &RuntimeHandle, f: F) -> Pid
where
    F: FnOnce(Context) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle.spawn(f)
}

/// Spawns a new process linked to the parent.
///
/// # Example
///
/// ```ignore
/// let child = spawn_link(&handle, parent_pid, |ctx| async move {
///     println!("Linked child process");
/// });
/// ```
pub fn spawn_link<F, Fut>(handle: &RuntimeHandle, parent: Pid, f: F) -> Pid
where
    F: FnOnce(Context) -> Fut + Send + 'static,
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
/// let (child, ref) = spawn_monitor(&handle, monitor_pid, |ctx| async move {
///     println!("Monitored child process");
/// });
/// ```
pub fn spawn_monitor<F, Fut>(handle: &RuntimeHandle, monitor: Pid, f: F) -> (Pid, Ref)
where
    F: FnOnce(Context) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    handle.spawn_monitor(monitor, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Runtime;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_spawn_function() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let _pid = spawn(&handle, move |_ctx| async move {
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
            spawn(&handle, move |_ctx| async move {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });
        }

        sleep(Duration::from_millis(100)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 10);
    }
}
