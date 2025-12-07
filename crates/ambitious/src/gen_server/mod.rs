//! # GenServer Pattern
//!
//! GenServer is a process abstraction for building stateful, message-driven actors.
//! The struct IS the process - `init` constructs it, and handlers mutate it via `&mut self`.
//!
//! # Design
//!
//! - **Enum-based message dispatch**: Define `Call`, `Cast`, `Info` message enums
//! - **OTP envelope compatibility**: Messages wrapped in `$gen_call`/`$gen_cast` format
//! - **Self-as-state**: The struct implementing `GenServer` IS the state
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::*;
//!
//! struct Counter {
//!     count: i64,
//! }
//!
//! #[derive(Message)]
//! enum CounterCall {
//!     Get,
//!     Reset(i64),
//! }
//!
//! #[derive(Message)]
//! enum CounterCast {
//!     Increment,
//!     Decrement,
//! }
//!
//! impl GenServer for Counter {
//!     type Args = i64;
//!     type Call = CounterCall;
//!     type Cast = CounterCast;
//!     type Info = ();
//!     type Reply = i64;
//!
//!     async fn init(initial: i64) -> Init<Self> {
//!         Init::Ok(Counter { count: initial })
//!     }
//!
//!     async fn handle_call(&mut self, msg: CounterCall, _from: From) -> Reply<i64> {
//!         match msg {
//!             CounterCall::Get => Reply::Ok(self.count),
//!             CounterCall::Reset(val) => {
//!                 let old = self.count;
//!                 self.count = val;
//!                 Reply::Ok(old)
//!             }
//!         }
//!     }
//!
//!     async fn handle_cast(&mut self, msg: CounterCast) -> Status {
//!         match msg {
//!             CounterCast::Increment => self.count += 1,
//!             CounterCast::Decrement => self.count -= 1,
//!         }
//!         Status::Ok
//!     }
//!
//!     async fn handle_info(&mut self, _msg: ()) -> Status {
//!         Status::Ok
//!     }
//! }
//! ```
//!
//! # Client API
//!
//! ```ignore
//! // Start the server
//! let pid = start::<Counter>(0).await?;
//!
//! // Make a typed call
//! let count: i64 = call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5)).await?;
//!
//! // Send a typed cast
//! cast::<Counter>(pid, CounterCast::Increment);
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

pub(crate) mod protocol;
mod types;

/// GenServer v3 - Enum-based message dispatch with OTP envelope compatibility.
///
/// This is the primary GenServer implementation.
pub mod v3;

// Re-export v3 as the primary API
pub use v3::{call, cast, start, start_link, Error, GenServer};

pub use async_trait::async_trait;
pub use protocol::From;
pub use types::{Init, Reply, Status};

// Re-export core types for convenience
pub use crate::core::{ExitReason, Pid, Ref, Term};

/// Prelude module for convenient imports.
///
/// Import everything needed to implement a GenServer with:
/// ```ignore
/// use ambitious::gen_server::prelude::*;
/// ```
pub mod prelude {
    pub use super::{
        Error, ExitReason, From, GenServer, Init, Pid, Reply, Status, async_trait, call, cast,
        start, start_link,
    };
}
