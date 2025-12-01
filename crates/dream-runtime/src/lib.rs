//! # dream-runtime
//!
//! Runtime infrastructure for DREAM (Distributed Rust Erlang Abstract Machine).
//!
//! This crate provides the core runtime components:
//!
//! - [`ProcessRegistry`] - Concurrent registry mapping PIDs to process handles
//! - [`Mailbox`] - Message queue for process communication
//! - [`ProcessHandle`] - Handle for interacting with a running process
//! - [`Context`] - Process execution context with access to runtime services

#![deny(warnings)]
#![deny(missing_docs)]
#![allow(dead_code)] // TODO: Remove once all pub APIs are implemented

mod context;
mod error;
mod mailbox;
mod process_handle;
mod registry;

pub use context::Context;
pub use error::{RuntimeError, SendError, SpawnError};
pub use mailbox::{Mailbox, MailboxSender};
pub use process_handle::{ProcessHandle, ProcessState};
pub use registry::ProcessRegistry;

// Re-export core types for convenience
pub use dream_core::{ExitReason, Message, Pid, Ref, SystemMessage};
