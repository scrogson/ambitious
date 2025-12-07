//! # ambitious-process
//!
//! Process primitives for Ambitious (Distributed Rust Erlang Abstract Machine).
//!
//! This crate provides the Process module API, mirroring Elixir's `Process`:
//!
//! - Spawning: `spawn`, `spawn_link`, `spawn_monitor`
//! - Links: `link`, `unlink`
//! - Monitors: `monitor`, `demonitor`
//! - Messaging: `send`, `send_after`
//! - Exit signals: `exit`
//! - Process info: `alive`, `self_pid`
//! - Registration: `register`, `whereis`, `unregister`
//!
//! # Example
//!
//! ```ignore
//! use ambitious::process::{spawn, send, self_pid};
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
pub use spawn::{ProcessFn, spawn, spawn_link, spawn_monitor};

// Re-export core types
pub use crate::core::{ExitReason, Pid, Ref, SystemMessage, Term};
pub use crate::runtime::{Context, ProcessRegistry, SendError};
