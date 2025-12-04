//! Application error types.

use std::fmt;

/// Error returned when starting an application fails.
#[derive(Debug, Clone)]
pub enum StartError {
    /// The application was not found.
    NotFound(String),
    /// The application is already running.
    AlreadyRunning(String),
    /// A dependency failed to start.
    DependencyFailed(String, String),
    /// The application's start callback failed.
    StartFailed(String),
    /// Circular dependency detected.
    CircularDependency(Vec<String>),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::NotFound(name) => write!(f, "application '{}' not found", name),
            StartError::AlreadyRunning(name) => {
                write!(f, "application '{}' is already running", name)
            }
            StartError::DependencyFailed(app, dep) => {
                write!(f, "dependency '{}' failed to start for '{}'", dep, app)
            }
            StartError::StartFailed(msg) => write!(f, "application failed to start: {}", msg),
            StartError::CircularDependency(cycle) => {
                write!(f, "circular dependency detected: {}", cycle.join(" -> "))
            }
        }
    }
}

impl std::error::Error for StartError {}

/// Error returned when stopping an application fails.
#[derive(Debug, Clone)]
pub enum StopError {
    /// The application was not found.
    NotFound(String),
    /// The application is not running.
    NotRunning(String),
    /// The stop timed out.
    Timeout,
}

impl fmt::Display for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopError::NotFound(name) => write!(f, "application '{}' not found", name),
            StopError::NotRunning(name) => write!(f, "application '{}' is not running", name),
            StopError::Timeout => write!(f, "stop timed out"),
        }
    }
}

impl std::error::Error for StopError {}
