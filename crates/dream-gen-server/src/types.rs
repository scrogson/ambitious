//! GenServer types and result enums.
//!
//! These types mirror Elixir's GenServer return values.

use dream_core::{ExitReason, Pid, Ref};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A handle identifying a pending call that needs a reply.
///
/// This is passed to `handle_call` and can be used to send
/// deferred replies via `GenServer::reply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct From {
    /// The PID of the calling process.
    pub caller: Pid,
    /// The unique reference for this call.
    pub reference: Ref,
}

impl From {
    /// Creates a new From handle.
    pub fn new(caller: Pid, reference: Ref) -> Self {
        Self { caller, reference }
    }
}

/// Continue argument for `handle_continue` callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinueArg(pub Vec<u8>);

impl ContinueArg {
    /// Creates a new continue argument from serializable data.
    pub fn new<T: Serialize>(data: &T) -> Self {
        Self(postcard::to_allocvec(data).unwrap_or_default())
    }

    /// Decodes the continue argument into the expected type.
    pub fn decode<T: for<'de> Deserialize<'de>>(&self) -> Result<T, postcard::Error> {
        postcard::from_bytes(&self.0)
    }
}

/// Result of the `init` callback.
#[derive(Debug)]
pub enum InitResult<S> {
    /// Initialization succeeded with the given state.
    Ok(S),
    /// Initialization succeeded; a timeout message will be sent.
    OkTimeout(S, Duration),
    /// Initialization succeeded; process should hibernate.
    OkHibernate(S),
    /// Initialization succeeded; `handle_continue` will be called.
    OkContinue(S, ContinueArg),
    /// Initialization ignored; process will exit normally.
    Ignore,
    /// Initialization failed; process will exit with the given reason.
    Stop(ExitReason),
}

impl<S> InitResult<S> {
    /// Creates a successful init result.
    pub fn ok(state: S) -> Self {
        InitResult::Ok(state)
    }

    /// Creates a successful init result with a timeout.
    pub fn ok_timeout(state: S, timeout: Duration) -> Self {
        InitResult::OkTimeout(state, timeout)
    }

    /// Creates an init result that triggers handle_continue.
    pub fn ok_continue<T: Serialize>(state: S, arg: &T) -> Self {
        InitResult::OkContinue(state, ContinueArg::new(arg))
    }

    /// Creates an init result that stops the server.
    pub fn stop(reason: ExitReason) -> Self {
        InitResult::Stop(reason)
    }

    /// Creates an ignored init result.
    pub fn ignore() -> Self {
        InitResult::Ignore
    }
}

/// Result of the `handle_call` callback.
#[derive(Debug)]
pub enum CallResult<S, R> {
    /// Reply to the caller and continue with new state.
    Reply(R, S),
    /// Reply to the caller, set timeout, continue with new state.
    ReplyTimeout(R, S, Duration),
    /// Reply and trigger `handle_continue`.
    ReplyContinue(R, S, ContinueArg),
    /// Don't reply yet (caller will wait); continue with new state.
    NoReply(S),
    /// Don't reply yet; set timeout.
    NoReplyTimeout(S, Duration),
    /// Don't reply yet; trigger `handle_continue`.
    NoReplyContinue(S, ContinueArg),
    /// Reply and stop the server.
    Stop(ExitReason, R, S),
    /// Stop the server without replying.
    StopNoReply(ExitReason, S),
}

impl<S, R> CallResult<S, R> {
    /// Creates a reply result.
    pub fn reply(reply: R, state: S) -> Self {
        CallResult::Reply(reply, state)
    }

    /// Creates a reply result with a timeout.
    pub fn reply_timeout(reply: R, state: S, timeout: Duration) -> Self {
        CallResult::ReplyTimeout(reply, state, timeout)
    }

    /// Creates a reply result that triggers handle_continue.
    pub fn reply_continue<T: Serialize>(reply: R, state: S, arg: &T) -> Self {
        CallResult::ReplyContinue(reply, state, ContinueArg::new(arg))
    }

    /// Creates a no-reply result.
    pub fn noreply(state: S) -> Self {
        CallResult::NoReply(state)
    }

    /// Creates a no-reply result with a timeout.
    pub fn noreply_timeout(state: S, timeout: Duration) -> Self {
        CallResult::NoReplyTimeout(state, timeout)
    }

    /// Creates a stop result with a reply.
    pub fn stop(reason: ExitReason, reply: R, state: S) -> Self {
        CallResult::Stop(reason, reply, state)
    }

    /// Creates a stop result without a reply.
    pub fn stop_noreply(reason: ExitReason, state: S) -> Self {
        CallResult::StopNoReply(reason, state)
    }
}

/// Result of the `handle_cast` callback.
#[derive(Debug)]
pub enum CastResult<S> {
    /// Continue with the new state.
    NoReply(S),
    /// Continue with new state and set a timeout.
    NoReplyTimeout(S, Duration),
    /// Continue and trigger `handle_continue`.
    NoReplyContinue(S, ContinueArg),
    /// Stop the server.
    Stop(ExitReason, S),
}

impl<S> CastResult<S> {
    /// Creates a no-reply result.
    pub fn noreply(state: S) -> Self {
        CastResult::NoReply(state)
    }

    /// Creates a no-reply result with a timeout.
    pub fn noreply_timeout(state: S, timeout: Duration) -> Self {
        CastResult::NoReplyTimeout(state, timeout)
    }

    /// Creates a no-reply result that triggers handle_continue.
    pub fn noreply_continue<T: Serialize>(state: S, arg: &T) -> Self {
        CastResult::NoReplyContinue(state, ContinueArg::new(arg))
    }

    /// Creates a stop result.
    pub fn stop(reason: ExitReason, state: S) -> Self {
        CastResult::Stop(reason, state)
    }
}

/// Result of the `handle_info` callback.
pub type InfoResult<S> = CastResult<S>;

/// Result of the `handle_continue` callback.
pub type ContinueResult<S> = CastResult<S>;

/// A reference to a GenServer.
///
/// Can be either a PID or a registered name.
#[derive(Debug, Clone)]
pub enum ServerRef {
    /// A direct process identifier.
    Pid(Pid),
    /// A registered name.
    Name(String),
}

impl std::convert::From<Pid> for ServerRef {
    fn from(pid: Pid) -> Self {
        ServerRef::Pid(pid)
    }
}

impl std::convert::From<&str> for ServerRef {
    fn from(name: &str) -> Self {
        ServerRef::Name(name.to_string())
    }
}

impl std::convert::From<String> for ServerRef {
    fn from(name: String) -> Self {
        ServerRef::Name(name)
    }
}
