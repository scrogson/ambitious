//! # ambitious-core
//!
//! Core types for Ambitious (Distributed Rust Erlang Abstract Machine).
//!
//! This crate provides the foundational types used throughout the Ambitious ecosystem:
//!
//! - [`Atom`] - Interned string for efficient comparison
//! - [`Pid`] - Process identifier
//! - [`Ref`] - Unique reference for monitors and timers
//! - [`ExitReason`] - Process termination reasons
//! - [`Term`] - Trait for Erlang-like serializable terms (messages, keys, etc.)
//! - [`SystemMessage`] - Internal system messages (Exit, Down, Timeout)
//! - [`NodeId`], [`NodeName`], [`NodeInfo`] - Node identity for distribution

#![deny(warnings)]
#![deny(missing_docs)]

pub mod etf;
mod exit_reason;
mod message;
pub mod node;
mod pid;
mod reference;
mod system_message;

// Re-export Atom from our atom module
pub use crate::atom::Atom;
// Note: atom! macro is exported at crate root via #[macro_export]

pub use exit_reason::ExitReason;
#[allow(deprecated)]
pub use message::{DecodeError, Message, RawTerm, Term};
pub use node::{NodeId, NodeInfo, NodeName};
pub use pid::{Pid, current_creation, increment_creation};
pub use reference::Ref;
pub use system_message::SystemMessage;
