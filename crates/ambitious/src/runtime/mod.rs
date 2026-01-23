//! # ambitious-runtime
//!
//! Runtime infrastructure for Ambitious (Distributed Rust Erlang Abstract Machine).
//!
//! This crate provides the core runtime components:
//!
//! - `ProcessRegistry` - Concurrent registry mapping PIDs to process handles
//! - `Mailbox` - Message queue for process communication
//! - `ProcessHandle` - Handle for interacting with a running process
//! - `Context` - Process execution context with access to runtime services

#![deny(warnings)]
#![deny(missing_docs)]
#![allow(dead_code)] // TODO: Remove once all pub APIs are implemented

mod context;
mod error;
mod mailbox;
mod process_handle;
mod registry;
mod task_local;

pub use context::Context;
pub use error::{RuntimeError, SendError, SpawnError};
pub use mailbox::{Mailbox, MailboxSender};
pub use process_handle::{ProcessHandle, ProcessState};
pub use registry::{ProcessRegistry, RemoteSendHook, set_remote_send_hook};
pub use task_local::{
    ProcessScope, current_pid, exit, recv, recv_timeout, send, send_raw, set_exit_reason,
    set_exit_reason_async, try_current_pid, try_recv, with_ctx, with_ctx_async,
};

// Re-export core types for convenience
pub use crate::core::{ExitReason, Pid, Ref, SystemMessage, Term};
