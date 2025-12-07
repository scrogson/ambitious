//! GenServer traits.
//!
//! The struct IS the process. `init` constructs it, handlers mutate it via `&mut self`.
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::*;
//! use ambitious::{call, Message};
//!
//! #[derive(Message)]
//! struct Get;
//!
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
//! #[call]
//! impl HandleCall<Get> for Counter {
//!     type Reply = i64;
//!     type Output = Reply<i64>;
//!
//!     async fn handle_call(&mut self, _msg: Get, _from: From) -> Reply<i64> {
//!         Reply::Ok(self.count)
//!     }
//! }
//! ```

use super::dispatch::HandlerRegistry;
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
    /// Override this to dispatch to typed `HandleCall<M>` handlers.
    /// The default implementation returns an error.
    async fn handle_call_raw(&mut self, _payload: Vec<u8>, _from: From) -> RawReply {
        RawReply::StopNoReply(ExitReason::Error("handle_call not implemented".into()))
    }

    /// Handle a raw cast message.
    ///
    /// Override this to dispatch to typed `HandleCast<M>` handlers.
    /// The default implementation ignores the message.
    async fn handle_cast_raw(&mut self, _payload: Vec<u8>) -> Status {
        Status::Ok
    }

    /// Handle a raw info message.
    ///
    /// Override this to dispatch to typed `HandleInfo<M>` handlers.
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

    /// Handle a raw continue message.
    ///
    /// Override this to dispatch to typed `HandleContinue<M>` handlers.
    /// The default implementation does nothing.
    async fn handle_continue_raw(&mut self, _arg: Vec<u8>) -> Status {
        Status::Ok
    }
}

/// Handle a synchronous call (request/response pattern).
///
/// Implement this trait for each message type your GenServer handles
/// via the call pattern. The caller blocks until a reply is sent.
///
/// The `Output` associated type allows returning either:
/// - `Reply<T>` - Direct reply
/// - `Result<Reply<T>, E>` - Errors stop the process (Erlang "let it crash")
///
/// # Example
///
/// ```ignore
/// struct GetCount;
///
/// // Simple handler - returns Reply directly
/// #[call]
/// impl HandleCall<GetCount> for Counter {
///     type Reply = i64;
///     type Output = Reply<i64>;
///
///     async fn handle_call(&mut self, _msg: GetCount, _from: From) -> Reply<i64> {
///         Reply::Ok(self.count)
///     }
/// }
///
/// // Fallible handler - returns Result for ? operator support
/// #[call]
/// impl HandleCall<FetchData> for DataServer {
///     type Reply = Data;
///     type Output = Result<Reply<Data>, MyError>;
///
///     async fn handle_call(&mut self, _msg: FetchData, _from: From) -> Result<Reply<Data>, MyError> {
///         let raw = self.db.fetch()?;  // Uses ? operator
///         let data = parse(raw)?;
///         Ok(Reply::Ok(data))
///     }
/// }
/// ```
#[async_trait]
pub trait HandleCall<M: Send + 'static>: GenServer {
    /// The type returned to the caller.
    type Reply: Send + 'static;

    /// The return type of the handler. Can be `Reply<Self::Reply>` or
    /// `Result<Reply<Self::Reply>, E>`. Errors cause the process to stop.
    type Output: Into<Reply<Self::Reply>> + Send;

    /// Handle the call message and return a reply.
    async fn handle_call(&mut self, msg: M, from: From) -> Self::Output;
}

/// Handle an asynchronous cast (fire-and-forget pattern).
///
/// Implement this trait for each message type your GenServer handles
/// via the cast pattern. The sender does not wait for a response.
///
/// The `Output` associated type allows returning either:
/// - `Status` - Direct status
/// - `Result<Status, E>` - Errors stop the process (Erlang "let it crash")
///
/// # Example
///
/// ```ignore
/// struct Increment;
///
/// // Simple handler
/// #[cast]
/// impl HandleCast<Increment> for Counter {
///     type Output = Status;
///
///     async fn handle_cast(&mut self, _msg: Increment) -> Status {
///         self.count += 1;
///         Status::Ok
///     }
/// }
///
/// // Fallible handler
/// #[cast]
/// impl HandleCast<ProcessData> for DataServer {
///     type Output = Result<Status, MyError>;
///
///     async fn handle_cast(&mut self, msg: ProcessData) -> Result<Status, MyError> {
///         self.process(msg.data)?;
///         Ok(Status::Ok)
///     }
/// }
/// ```
#[async_trait]
pub trait HandleCast<M: Send + 'static>: GenServer {
    /// The return type of the handler. Can be `Status` or `Result<Status, E>`.
    type Output: Into<Status> + Send;

    /// Handle the cast message.
    async fn handle_cast(&mut self, msg: M) -> Self::Output;
}

/// Handle info messages (system messages, timers, monitors, etc.).
///
/// Implement this trait for each info message type your GenServer handles.
/// Info messages come from the runtime (timeouts, monitors) or other processes
/// sending directly to your mailbox.
///
/// The `Output` associated type allows returning either:
/// - `Status` - Direct status
/// - `Result<Status, E>` - Errors stop the process (Erlang "let it crash")
///
/// # Example
///
/// ```ignore
/// struct Tick;
///
/// #[info]
/// impl HandleInfo<Tick> for Counter {
///     type Output = Status;
///
///     async fn handle_info(&mut self, _msg: Tick) -> Status {
///         println!("tick at {}", self.count);
///         Status::Timeout(Duration::from_secs(1))
///     }
/// }
/// ```
#[async_trait]
pub trait HandleInfo<M: Send + 'static>: GenServer {
    /// The return type of the handler. Can be `Status` or `Result<Status, E>`.
    type Output: Into<Status> + Send;

    /// Handle the info message.
    async fn handle_info(&mut self, msg: M) -> Self::Output;
}

/// Handle continue messages (for multi-step initialization or deferred work).
///
/// Implement this trait for each continue message type your GenServer handles.
/// Continue messages are triggered by returning `Init::Continue` or `Status::Continue`.
///
/// The `Output` associated type allows returning either:
/// - `Status` - Direct status
/// - `Result<Status, E>` - Errors stop the process (Erlang "let it crash")
///
/// # Example
///
/// ```ignore
/// struct SetupDatabase;
///
/// impl GenServer for MyServer {
///     type Args = Config;
///
///     async fn init(config: Config) -> Init<Self> {
///         // Start immediately, defer heavy setup to handle_continue
///         Init::Continue(MyServer { config }, SetupDatabase.encode_local())
///     }
/// }
///
/// #[cont]
/// impl HandleContinue<SetupDatabase> for MyServer {
///     type Output = Status;
///
///     async fn handle_continue(&mut self, _msg: SetupDatabase) -> Status {
///         // Do heavy initialization here
///         self.db = Database::connect(&self.config.db_url).await;
///         Status::Ok
///     }
/// }
/// ```
#[async_trait]
pub trait HandleContinue<M: Send + 'static>: GenServer {
    /// The return type of the handler. Can be `Status` or `Result<Status, E>`.
    type Output: Into<Status> + Send;

    /// Handle the continue message.
    async fn handle_continue(&mut self, msg: M) -> Self::Output;
}

/// Trait for GenServers with registered handlers.
///
/// Implement this trait (via the `register_handlers!` macro) to enable
/// automatic message dispatch without manual `handle_call_raw` implementations.
///
/// # Example
///
/// ```ignore
/// register_handlers!(Counter {
///     calls: [Get, Reset],
///     casts: [Increment],
/// });
/// ```
pub trait HasHandlers: GenServer {
    /// Get the handler registry for this GenServer type.
    fn registry() -> &'static HandlerRegistry<Self>;

    /// Dispatch a call message using the registry.
    fn handle_call_dispatch(
        server: &mut Self,
        payload: Vec<u8>,
        from: From,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RawReply> + Send + '_>>;

    /// Dispatch a cast message using the registry.
    fn handle_cast_dispatch(
        server: &mut Self,
        payload: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Status> + Send + '_>>;

    /// Dispatch an info message using the registry.
    fn handle_info_dispatch(
        server: &mut Self,
        payload: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Status> + Send + '_>>;

    /// Dispatch a continue message using the registry.
    fn handle_continue_dispatch(
        server: &mut Self,
        payload: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Status> + Send + '_>>;
}
