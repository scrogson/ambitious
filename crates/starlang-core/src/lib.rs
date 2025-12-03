//! # starlang-core
//!
//! Core types for Starlang (Distributed Rust Erlang Abstract Machine).
//!
//! This crate provides the foundational types used throughout the Starlang ecosystem:
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

mod exit_reason;
mod message;
pub mod node;
mod pid;
mod reference;
mod system_message;

// Re-export Atom from dream-atom for convenience
pub use starlang_atom::{atom, Atom};

pub use exit_reason::ExitReason;
#[allow(deprecated)]
pub use message::{DecodeError, Message, Term};
pub use node::{NodeId, NodeInfo, NodeName};
pub use pid::{current_creation, increment_creation, Pid};
pub use reference::Ref;
pub use system_message::SystemMessage;
