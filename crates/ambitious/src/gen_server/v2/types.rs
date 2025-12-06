//! GenServer v2 result types.
//!
//! Clean, minimal enums for GenServer callbacks.

use crate::core::ExitReason;
use std::time::Duration;

/// Result from `GenServer::init`.
///
/// Determines how the process starts (or doesn't).
#[derive(Debug)]
pub enum Init<S> {
    /// Process starts with this state.
    Ok(S),
    /// Process starts, then immediately triggers `handle_continue`.
    Continue(S, Vec<u8>),
    /// Process starts, then receives a timeout message after the duration.
    Timeout(S, Duration),
    /// Don't start the process. Not an error, just skip.
    /// Useful for conditional process creation.
    Ignore,
    /// Failed to start. Process terminates immediately.
    Stop(ExitReason),
}

impl<S> Init<S> {
    /// Create an `Ok` result.
    pub fn ok(state: S) -> Self {
        Init::Ok(state)
    }

    /// Create a `Continue` result with serialized continue argument.
    pub fn cont(state: S, arg: Vec<u8>) -> Self {
        Init::Continue(state, arg)
    }

    /// Create a `Timeout` result.
    pub fn timeout(state: S, duration: Duration) -> Self {
        Init::Timeout(state, duration)
    }

    /// Create an `Ignore` result.
    pub fn ignore() -> Self {
        Init::Ignore
    }

    /// Create a `Stop` result.
    pub fn stop(reason: ExitReason) -> Self {
        Init::Stop(reason)
    }
}

/// Result from `Call::call`.
///
/// Must either reply to the caller or explicitly defer the reply.
#[derive(Debug)]
pub enum Reply<T> {
    /// Send reply to caller, continue running.
    Ok(T),
    /// Send reply, then trigger `handle_continue`.
    Continue(T, Vec<u8>),
    /// Send reply, then set timeout for next message.
    Timeout(T, Duration),
    /// Don't reply yet. Must call `GenServer::reply` later.
    /// Caller remains blocked until reply is sent.
    NoReply,
    /// Stop the process after sending reply.
    Stop(ExitReason, T),
    /// Stop the process without sending reply.
    /// Caller will receive an error.
    StopNoReply(ExitReason),
}

impl<T> Reply<T> {
    /// Create an `Ok` reply.
    pub fn ok(value: T) -> Self {
        Reply::Ok(value)
    }

    /// Create a `Continue` reply.
    pub fn cont(value: T, arg: Vec<u8>) -> Self {
        Reply::Continue(value, arg)
    }

    /// Create a `Timeout` reply.
    pub fn timeout(value: T, duration: Duration) -> Self {
        Reply::Timeout(value, duration)
    }

    /// Create a `NoReply`.
    pub fn noreply() -> Self {
        Reply::NoReply
    }

    /// Create a `Stop` reply.
    pub fn stop(reason: ExitReason, value: T) -> Self {
        Reply::Stop(reason, value)
    }

    /// Create a `StopNoReply`.
    pub fn stop_noreply(reason: ExitReason) -> Self {
        Reply::StopNoReply(reason)
    }
}

/// Result from `Cast::cast` and `Info::info`.
///
/// No reply is possible - these are fire-and-forget operations.
#[derive(Debug, Default)]
pub enum Status {
    /// Continue running normally.
    #[default]
    Ok,
    /// Trigger `handle_continue` with the given argument.
    Continue(Vec<u8>),
    /// Set timeout - receive timeout message after duration.
    Timeout(Duration),
    /// Stop the process.
    Stop(ExitReason),
}

impl Status {
    /// Create an `Ok` status.
    pub fn ok() -> Self {
        Status::Ok
    }

    /// Create a `Continue` status.
    pub fn cont(arg: Vec<u8>) -> Self {
        Status::Continue(arg)
    }

    /// Create a `Timeout` status.
    pub fn timeout(duration: Duration) -> Self {
        Status::Timeout(duration)
    }

    /// Create a `Stop` status.
    pub fn stop(reason: ExitReason) -> Self {
        Status::Stop(reason)
    }
}
