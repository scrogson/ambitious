//! Supervisor error types.

use std::fmt;

/// Error returned when starting a supervisor fails.
#[derive(Debug, Clone)]
pub enum StartError {
    /// The supervisor's init callback failed.
    InitFailed(String),
    /// Failed to spawn the supervisor process.
    SpawnFailed(String),
    /// A child failed to start during initialization.
    ChildFailed(String, String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::InitFailed(msg) => write!(f, "supervisor init failed: {}", msg),
            StartError::SpawnFailed(msg) => write!(f, "supervisor spawn failed: {}", msg),
            StartError::ChildFailed(id, msg) => {
                write!(f, "child '{}' failed to start: {}", id, msg)
            }
        }
    }
}

impl std::error::Error for StartError {}

/// Error returned when terminating a child fails.
#[derive(Debug, Clone)]
pub enum TerminateError {
    /// The child was not found.
    NotFound(String),
    /// The termination timed out.
    Timeout,
}

impl fmt::Display for TerminateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminateError::NotFound(id) => write!(f, "child '{}' not found", id),
            TerminateError::Timeout => write!(f, "termination timed out"),
        }
    }
}

impl std::error::Error for TerminateError {}

/// Error returned when restarting a child fails.
#[derive(Debug, Clone)]
pub enum RestartError {
    /// The child was not found.
    NotFound(String),
    /// The child is already running.
    AlreadyRunning(String),
    /// The child failed to start.
    StartFailed(String),
}

impl fmt::Display for RestartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestartError::NotFound(id) => write!(f, "child '{}' not found", id),
            RestartError::AlreadyRunning(id) => write!(f, "child '{}' is already running", id),
            RestartError::StartFailed(msg) => write!(f, "child failed to start: {}", msg),
        }
    }
}

impl std::error::Error for RestartError {}

/// Error returned when deleting a child fails.
#[derive(Debug, Clone)]
pub enum DeleteError {
    /// The child was not found.
    NotFound(String),
    /// The child is still running.
    Running(String),
}

impl fmt::Display for DeleteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeleteError::NotFound(id) => write!(f, "child '{}' not found", id),
            DeleteError::Running(id) => write!(f, "child '{}' is still running", id),
        }
    }
}

impl std::error::Error for DeleteError {}
