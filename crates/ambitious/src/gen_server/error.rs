//! GenServer error types.

use crate::core::{ExitReason, Pid};
use std::fmt;

/// Error returned when starting a GenServer fails.
#[derive(Debug, Clone)]
pub enum StartError {
    /// The init callback returned `Ignore`.
    Ignore,
    /// The init callback returned `Stop` with the given reason.
    Stop(ExitReason),
    /// The init callback timed out.
    Timeout,
    /// Failed to spawn the process.
    SpawnFailed(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Ignore => write!(f, "init returned :ignore"),
            StartError::Stop(reason) => write!(f, "init stopped: {}", reason),
            StartError::Timeout => write!(f, "init timed out"),
            StartError::SpawnFailed(msg) => write!(f, "spawn failed: {}", msg),
        }
    }
}

impl std::error::Error for StartError {}

/// Error returned when a GenServer call fails.
#[derive(Debug, Clone)]
pub enum CallError {
    /// The server process is not alive.
    NotAlive(Pid),
    /// The call timed out.
    Timeout,
    /// The server exited during the call.
    Exit(ExitReason),
    /// The server name could not be resolved.
    NotFound(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::NotAlive(pid) => write!(f, "process {} is not alive", pid),
            CallError::Timeout => write!(f, "call timed out"),
            CallError::Exit(reason) => write!(f, "server exited: {}", reason),
            CallError::NotFound(name) => write!(f, "server not found: {}", name),
        }
    }
}

impl std::error::Error for CallError {}

/// Error returned when stopping a GenServer fails.
#[derive(Debug, Clone)]
pub enum StopError {
    /// The server process is not alive.
    NotAlive(Pid),
    /// The stop timed out.
    Timeout,
    /// The server name could not be resolved.
    NotFound(String),
}

impl fmt::Display for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopError::NotAlive(pid) => write!(f, "process {} is not alive", pid),
            StopError::Timeout => write!(f, "stop timed out"),
            StopError::NotFound(name) => write!(f, "server not found: {}", name),
        }
    }
}

impl std::error::Error for StopError {}
