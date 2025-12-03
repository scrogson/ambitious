//! # starlang-process
//!
//! Process primitives for Starlang (Distributed Rust Erlang Abstract Machine).
//!
//! This crate provides the Process module API, mirroring Elixir's `Process`:
//!
//! - Spawning: [`spawn`], [`spawn_link`], [`spawn_monitor`]
//! - Links: [`link`], [`unlink`]
//! - Monitors: [`monitor`], [`demonitor`]
//! - Messaging: [`send`], [`send_after`]
//! - Exit signals: [`exit`]
//! - Process info: [`alive`], [`self_pid`]
//! - Registration: [`register`], [`whereis`], [`unregister`]
//!
//! # Example
//!
//! ```ignore
//! use starlang_process::{spawn, send, self_pid};
//!
//! // Spawn a new process
//! let pid = spawn(|ctx| async move {
//!     loop {
//!         if let Some(msg) = ctx.recv().await {
//!             println!("Received: {:?}", msg);
//!         }
//!     }
//! }).await?;
//!
//! // Send a message
//! send(pid, &"hello")?;
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

pub mod global;
mod runtime;
mod spawn;

pub use runtime::{Runtime, RuntimeHandle};
pub use spawn::{spawn, spawn_link, spawn_monitor, ProcessFn};

// Re-export core types
pub use starlang_core::{ExitReason, Pid, Ref, SystemMessage, Term};
pub use starlang_runtime::{Context, ProcessRegistry, SendError};
