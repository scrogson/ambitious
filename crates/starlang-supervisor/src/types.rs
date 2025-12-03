//! Supervisor types and configuration.
//!
//! These types define how supervisors manage their children.

use starlang_core::Pid;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Supervision strategy that determines how child failures are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    /// If a child process terminates, only that process is restarted.
    OneForOne,
    /// If a child process terminates, all other child processes are
    /// terminated and then all child processes are restarted.
    OneForAll,
    /// If a child process terminates, the terminated process and all
    /// children started after it are terminated and restarted.
    RestForOne,
}

impl Default for Strategy {
    fn default() -> Self {
        Strategy::OneForOne
    }
}

/// Determines when a child should be restarted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestartType {
    /// The child is always restarted, regardless of exit reason.
    Permanent,
    /// The child is restarted only if it terminates abnormally.
    Transient,
    /// The child is never restarted.
    Temporary,
}

impl Default for RestartType {
    fn default() -> Self {
        RestartType::Permanent
    }
}

/// Determines how a child should be terminated during shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShutdownType {
    /// The child is terminated immediately using an exit signal.
    BrutalKill,
    /// The child is given the specified duration to terminate gracefully.
    Timeout(Duration),
    /// The child can take as long as needed to terminate.
    Infinity,
}

impl Default for ShutdownType {
    fn default() -> Self {
        ShutdownType::Timeout(Duration::from_secs(5))
    }
}

/// The type of a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChildType {
    /// A worker process (leaf node in supervision tree).
    Worker,
    /// A supervisor process (internal node in supervision tree).
    Supervisor,
}

impl Default for ChildType {
    fn default() -> Self {
        ChildType::Worker
    }
}

/// Error returned when starting a child fails.
#[derive(Debug, Clone)]
pub enum StartChildError {
    /// The child failed to start.
    Failed(String),
    /// The child's init returned ignore.
    Ignore,
    /// A child with this ID already exists.
    AlreadyPresent,
}

impl std::fmt::Display for StartChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartChildError::Failed(msg) => write!(f, "child failed to start: {}", msg),
            StartChildError::Ignore => write!(f, "child init returned ignore"),
            StartChildError::AlreadyPresent => write!(f, "child already present"),
        }
    }
}

impl std::error::Error for StartChildError {}

/// A function that starts a child process.
pub type StartFn = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<Pid, StartChildError>> + Send>>
        + Send
        + Sync,
>;

/// Specification for a child process.
///
/// This defines how a child should be started, restarted, and terminated.
pub struct ChildSpec {
    /// Unique identifier for this child.
    pub id: String,
    /// Function to start the child. Returns the PID on success.
    pub start: StartFn,
    /// When the child should be restarted.
    pub restart: RestartType,
    /// How the child should be terminated.
    pub shutdown: ShutdownType,
    /// Whether this is a worker or supervisor.
    pub child_type: ChildType,
}

impl std::fmt::Debug for ChildSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildSpec")
            .field("id", &self.id)
            .field("restart", &self.restart)
            .field("shutdown", &self.shutdown)
            .field("child_type", &self.child_type)
            .finish()
    }
}

impl ChildSpec {
    /// Creates a new child specification with the given ID and start function.
    pub fn new<F, Fut>(id: impl Into<String>, start: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Pid, StartChildError>> + Send + 'static,
    {
        Self {
            id: id.into(),
            start: Arc::new(move || Box::pin(start())),
            restart: RestartType::default(),
            shutdown: ShutdownType::default(),
            child_type: ChildType::default(),
        }
    }

    /// Sets the restart type.
    pub fn restart(mut self, restart: RestartType) -> Self {
        self.restart = restart;
        self
    }

    /// Sets the shutdown type.
    pub fn shutdown(mut self, shutdown: ShutdownType) -> Self {
        self.shutdown = shutdown;
        self
    }

    /// Sets the child type.
    pub fn child_type(mut self, child_type: ChildType) -> Self {
        self.child_type = child_type;
        self
    }

    /// Marks this child as a worker with the given shutdown timeout.
    pub fn worker(self) -> Self {
        self.child_type(ChildType::Worker)
    }

    /// Marks this child as a supervisor.
    pub fn supervisor(self) -> Self {
        self.child_type(ChildType::Supervisor)
            .shutdown(ShutdownType::Infinity)
    }
}

/// Supervisor initialization flags.
#[derive(Debug, Clone)]
pub struct SupervisorFlags {
    /// The supervision strategy.
    pub strategy: Strategy,
    /// Maximum number of restarts allowed in the time period.
    pub max_restarts: u32,
    /// Time period in seconds for restart counting.
    pub max_seconds: u32,
}

impl Default for SupervisorFlags {
    fn default() -> Self {
        Self {
            strategy: Strategy::OneForOne,
            max_restarts: 3,
            max_seconds: 5,
        }
    }
}

impl SupervisorFlags {
    /// Creates new supervisor flags with the given strategy.
    pub fn new(strategy: Strategy) -> Self {
        Self {
            strategy,
            ..Default::default()
        }
    }

    /// Sets the maximum restarts.
    pub fn max_restarts(mut self, max: u32) -> Self {
        self.max_restarts = max;
        self
    }

    /// Sets the time period for restart counting.
    pub fn max_seconds(mut self, secs: u32) -> Self {
        self.max_seconds = secs;
        self
    }
}

/// Information about a running child.
#[derive(Debug, Clone)]
pub struct ChildInfo {
    /// The child's unique identifier.
    pub id: String,
    /// The child's PID, if running.
    pub pid: Option<Pid>,
    /// The child type.
    pub child_type: ChildType,
}

/// Statistics about supervisor children.
#[derive(Debug, Clone, Default)]
pub struct ChildCounts {
    /// Number of child specifications.
    pub specs: usize,
    /// Number of actively running children.
    pub active: usize,
    /// Number of supervisors (subset of active).
    pub supervisors: usize,
    /// Number of workers (subset of active).
    pub workers: usize,
}
