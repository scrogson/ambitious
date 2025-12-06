//! GenServer v2 traits.
//!
//! The struct IS the process. `init` constructs it, handlers mutate it via `&mut self`.
//!
//! # Example
//!
//! ```ignore
//! struct Counter {
//!     count: i64,
//! }
//!
//! impl GenServer for Counter {
//!     type Args = i64;
//!
//!     async fn init(initial: i64) -> Init<Self> {
//!         Init::Ok(Counter { count: initial })
//!     }
//! }
//!
//! impl Call<Get> for Counter {
//!     type Reply = i64;
//!
//!     async fn call(&mut self, _msg: Get, _from: From) -> Reply<i64> {
//!         Reply::Ok(self.count)
//!     }
//! }
//! ```

use super::protocol::From;
use super::server::RawReply;
use super::types::{Init, Reply, Status};
use crate::core::ExitReason;
pub use async_trait::async_trait;

/// Core GenServer trait - the struct IS the process.
///
/// Implement this trait to create a GenServer. The struct's fields
/// are the process state. `init` constructs the struct, and handlers
/// receive `&mut self` to mutate it.
#[async_trait]
pub trait GenServer: Sized + Send + 'static {
    /// Arguments passed to `start_link` and received by `init`.
    type Args: Send + 'static;

    /// Constructs the process. Called when the process spawns.
    ///
    /// Return `Init::Ok(self)` to start normally, or other variants
    /// for timeouts, continue actions, or to stop/ignore.
    async fn init(args: Self::Args) -> Init<Self>;

    /// Called when the process is terminating.
    ///
    /// Use this to clean up resources. The default implementation does nothing.
    async fn terminate(&mut self, _reason: ExitReason) {}

    /// Handle a raw call message.
    ///
    /// Override this to dispatch to typed `Call<M>` handlers.
    /// The default implementation returns an error.
    async fn handle_call_raw(&mut self, _payload: Vec<u8>, _from: From) -> RawReply {
        RawReply::StopNoReply(ExitReason::Error("handle_call not implemented".into()))
    }

    /// Handle a raw cast message.
    ///
    /// Override this to dispatch to typed `Cast<M>` handlers.
    /// The default implementation ignores the message.
    async fn handle_cast_raw(&mut self, _payload: Vec<u8>) -> Status {
        Status::Ok
    }

    /// Handle a raw info message.
    ///
    /// Override this to dispatch to typed `Info<M>` handlers.
    /// The default implementation ignores the message.
    async fn handle_info_raw(&mut self, _msg: Vec<u8>) -> Status {
        Status::Ok
    }

    /// Handle a timeout message.
    ///
    /// Override this if you use `Init::Timeout` or `Status::Timeout`.
    /// The default implementation does nothing.
    async fn handle_timeout(&mut self) -> Status {
        Status::Ok
    }

    /// Handle a continue message.
    ///
    /// Override this if you use `Init::Continue` or `Status::Continue`.
    /// The default implementation does nothing.
    async fn handle_continue(&mut self, _arg: Vec<u8>) -> Status {
        Status::Ok
    }
}

/// Handle a synchronous call (request/response pattern).
///
/// Implement this trait for each message type your GenServer handles
/// via the call pattern. The caller blocks until a reply is sent.
///
/// # Example
///
/// ```ignore
/// struct GetCount;
///
/// impl Call<GetCount> for Counter {
///     type Reply = i64;
///
///     async fn call(&mut self, _msg: GetCount, _from: From) -> Reply<i64> {
///         Reply::Ok(self.count)
///     }
/// }
/// ```
#[async_trait]
pub trait Call<M: Send + 'static>: GenServer {
    /// The type returned to the caller.
    type Reply: Send + 'static;

    /// Handle the call message and return a reply.
    async fn call(&mut self, msg: M, from: From) -> Reply<Self::Reply>;
}

/// Handle an asynchronous cast (fire-and-forget pattern).
///
/// Implement this trait for each message type your GenServer handles
/// via the cast pattern. The sender does not wait for a response.
///
/// # Example
///
/// ```ignore
/// struct Increment;
///
/// impl Cast<Increment> for Counter {
///     async fn cast(&mut self, _msg: Increment) -> Status {
///         self.count += 1;
///         Status::Ok
///     }
/// }
/// ```
#[async_trait]
pub trait Cast<M: Send + 'static>: GenServer {
    /// Handle the cast message.
    async fn cast(&mut self, msg: M) -> Status;
}

/// Handle info messages (system messages, timers, monitors, etc.).
///
/// Implement this trait for each info message type your GenServer handles.
/// Info messages come from the runtime (timeouts, monitors) or other processes
/// sending directly to your mailbox.
///
/// # Example
///
/// ```ignore
/// struct Tick;
///
/// impl Info<Tick> for Counter {
///     async fn info(&mut self, _msg: Tick) -> Status {
///         println!("tick at {}", self.count);
///         Status::Timeout(Duration::from_secs(1))
///     }
/// }
/// ```
#[async_trait]
pub trait Info<M: Send + 'static>: GenServer {
    /// Handle the info message.
    async fn info(&mut self, msg: M) -> Status;
}
