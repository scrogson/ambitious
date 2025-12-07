//! GenServer traits - enum-based message dispatch.
//!
//! The struct IS the process. `init` constructs it, handlers mutate it via `&mut self`.
//! Messages are organized into associated enum types for Call, Cast, and Info.

use super::protocol::From;
use super::types::{Init, Reply, Status};
use crate::core::ExitReason;
use crate::message::Message;
pub use async_trait::async_trait;

/// Core GenServer trait with enum-based message types.
///
/// # Design
///
/// Instead of implementing separate traits for each message type (`HandleCall<Get>`,
/// `HandleCall<Reset>`, etc.), you define enum types for your messages and implement
/// a single handler method that pattern-matches on them.
///
/// This design:
/// - Is simpler (no macro registration, no global dispatch tables)
/// - Is BEAM-compatible (messages wrapped in `$gen_call`/`$gen_cast` envelopes)
/// - Enables creating custom behaviors by following the same pattern
///
/// # Associated Types
///
/// - `Args`: Arguments passed to `init` when starting the server
/// - `Call`: Enum of all synchronous request message types
/// - `Cast`: Enum of all asynchronous message types
/// - `Info`: Enum of all info message types (system messages, timers, etc.)
/// - `Reply`: The reply type for call messages
///
/// # Example
///
/// ```ignore
/// struct Counter { count: i64 }
///
/// #[derive(Message)]
/// enum CounterCall {
///     Get,
///     Reset(i64),
/// }
///
/// #[derive(Message)]
/// enum CounterCast {
///     Increment,
/// }
///
/// impl GenServer for Counter {
///     type Args = i64;
///     type Call = CounterCall;
///     type Cast = CounterCast;
///     type Info = ();
///     type Reply = i64;
///
///     async fn init(initial: i64) -> Init<Self> {
///         Init::Ok(Counter { count: initial })
///     }
///
///     async fn handle_call(&mut self, msg: CounterCall, _from: From) -> Reply<i64> {
///         match msg {
///             CounterCall::Get => Reply::Ok(self.count),
///             CounterCall::Reset(val) => {
///                 let old = self.count;
///                 self.count = val;
///                 Reply::Ok(old)
///             }
///         }
///     }
///
///     async fn handle_cast(&mut self, msg: CounterCast) -> Status {
///         match msg {
///             CounterCast::Increment => {
///                 self.count += 1;
///                 Status::Ok
///             }
///         }
///     }
///
///     async fn handle_info(&mut self, _msg: ()) -> Status {
///         Status::Ok
///     }
/// }
/// ```
#[async_trait]
pub trait GenServer: Sized + Send + 'static {
    /// Arguments passed to `start` or `start_link` and received by `init`.
    type Args: Send + 'static;

    /// Enum of all synchronous call message types.
    ///
    /// This should be an enum that implements `Message`. Each variant represents
    /// a different type of synchronous request that the server can handle.
    type Call: Message + Send + 'static;

    /// Enum of all asynchronous cast message types.
    ///
    /// This should be an enum that implements `Message`. Each variant represents
    /// a different type of fire-and-forget message.
    type Cast: Message + Send + 'static;

    /// Enum of all info message types.
    ///
    /// Info messages come from the runtime (timeouts, monitors) or other processes
    /// sending directly to the mailbox without using `call` or `cast`.
    ///
    /// Use `()` if you don't handle any info messages.
    type Info: Message + Send + 'static;

    /// The reply type for call messages.
    ///
    /// This can be a single type or an enum if different calls return different types.
    type Reply: Message + Send + 'static;

    /// Constructs the process state. Called when the process spawns.
    ///
    /// Return `Init::Ok(self)` to start normally, or other variants
    /// for timeouts, continue actions, or to stop/ignore.
    async fn init(args: Self::Args) -> Init<Self>;

    /// Handle a synchronous call message.
    ///
    /// Pattern match on the `Call` enum to dispatch to the appropriate handler logic.
    /// Return a `Reply` to send the response to the caller.
    async fn handle_call(&mut self, msg: Self::Call, from: From) -> Reply<Self::Reply>;

    /// Handle an asynchronous cast message.
    ///
    /// Pattern match on the `Cast` enum to dispatch to the appropriate handler logic.
    /// Cast messages are fire-and-forget - there is no reply.
    async fn handle_cast(&mut self, msg: Self::Cast) -> Status;

    /// Handle an info message.
    ///
    /// Info messages come from the runtime (timeouts, monitors) or other processes.
    /// Pattern match on the `Info` enum to dispatch to the appropriate handler logic.
    async fn handle_info(&mut self, msg: Self::Info) -> Status;

    /// Called when the process is terminating.
    ///
    /// Use this to clean up resources. The default implementation does nothing.
    async fn terminate(&mut self, _reason: ExitReason) {}

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
    /// The `arg` is the raw bytes passed to the continue.
    async fn handle_continue(&mut self, _arg: Vec<u8>) -> Status {
        Status::Ok
    }
}
