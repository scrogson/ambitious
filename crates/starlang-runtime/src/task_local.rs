//! Task-local context for Starlang processes.
//!
//! This module provides task-local storage for the process context,
//! allowing functions to access the current process's context without
//! explicit parameter passing.

use crate::{Context, Pid, SendError};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Container for process context that provides cheap PID access.
struct ProcessContext {
    /// The process's PID (immutable after creation).
    pid: Pid,
    /// The process context (requires async lock for mutable access).
    ctx: Arc<Mutex<Context>>,
}

tokio::task_local! {
    /// Task-local storage for the current process context.
    static CONTEXT: ProcessContext;
}

/// Wrapper that sets up task-local context for a process.
///
/// This struct holds the context and sets up task-local storage
/// so that functions like `current_pid()`, `recv()`, etc. can access it.
pub struct ProcessScope {
    ctx: Context,
}

impl ProcessScope {
    /// Creates a new process scope with the given context.
    pub fn new(ctx: Context) -> Self {
        Self { ctx }
    }

    /// Runs the process function with task-local context available.
    ///
    /// Task-local functions like `current_pid()`, `recv()`, `send()`, etc.
    /// will work during execution.
    pub async fn run<F, Fut>(self, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let pid = self.ctx.pid();
        let process_ctx = ProcessContext {
            pid,
            ctx: Arc::new(Mutex::new(self.ctx)),
        };
        CONTEXT
            .scope(process_ctx, async {
                f().await;
            })
            .await;
    }
}

/// Gets the current process's PID from task-local context.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub fn current_pid() -> Pid {
    CONTEXT.with(|ctx| ctx.pid)
}

/// Gets the current process's PID, returning None if not in a process context.
pub fn try_current_pid() -> Option<Pid> {
    CONTEXT.try_with(|ctx| ctx.pid).ok()
}

/// Receives the next message from the current process's mailbox.
///
/// Returns `None` if the mailbox is closed.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub async fn recv() -> Option<Vec<u8>> {
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    ctx.lock().await.recv().await
}

/// Receives the next message with a timeout.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub async fn recv_timeout(timeout: Duration) -> Result<Option<Vec<u8>>, ()> {
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    ctx.lock().await.recv_timeout(timeout).await
}

/// Tries to receive a message without blocking.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub fn try_recv() -> Option<Vec<u8>> {
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    // Use try_lock since we're in a sync context
    match ctx.try_lock() {
        Ok(mut guard) => guard.try_recv(),
        Err(_) => None, // Lock is held, can't try_recv right now
    }
}

/// Sends a raw message to another process.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub fn send_raw(pid: Pid, data: Vec<u8>) -> Result<(), SendError> {
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    // Use try_lock since we're in a sync context
    match ctx.try_lock() {
        Ok(guard) => guard.send_raw(pid, data),
        Err(_) => Err(SendError::ProcessNotFound(pid)), // Lock is held
    }
}

/// Sends a typed message to another process.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub fn send<M: crate::Term>(pid: Pid, msg: &M) -> Result<(), SendError> {
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    // Use try_lock since we're in a sync context
    match ctx.try_lock() {
        Ok(guard) => guard.send(pid, msg),
        Err(_) => Err(SendError::ProcessNotFound(pid)), // Lock is held
    }
}

/// Executes a function with mutable access to the current context.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context or if
/// the context lock is already held.
pub fn with_ctx<F, R>(f: F) -> R
where
    F: FnOnce(&mut Context) -> R,
{
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    // Use try_lock since we're in a sync context
    match ctx.try_lock() {
        Ok(mut guard) => f(&mut guard),
        Err(_) => panic!("with_ctx called while context is already locked"),
    }
}

/// Executes an async function with mutable access to the current context.
///
/// # Panics
///
/// Panics if called outside of a Starlang process context.
pub async fn with_ctx_async<F, Fut, R>(f: F) -> R
where
    F: FnOnce(&mut Context) -> Fut,
    Fut: Future<Output = R>,
{
    let ctx = CONTEXT.with(|c| c.ctx.clone());
    let mut guard = ctx.lock().await;
    f(&mut guard).await
}
