//! Error types for runtime operations.

use crate::core::Pid;
use thiserror::Error;

/// Errors that can occur during runtime operations.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Process not found in registry.
    #[error("process not found: {0}")]
    ProcessNotFound(Pid),

    /// Failed to send a message.
    #[error("send failed: {0}")]
    SendFailed(#[from] SendError),

    /// Failed to spawn a process.
    #[error("spawn failed: {0}")]
    SpawnFailed(#[from] SpawnError),
}

/// Errors that can occur when sending messages.
#[derive(Debug, Error)]
pub enum SendError {
    /// The target process does not exist.
    #[error("process not found: {0}")]
    ProcessNotFound(Pid),

    /// The mailbox is full (bounded channels only).
    #[error("mailbox full")]
    MailboxFull,

    /// The process has terminated.
    #[error("process terminated")]
    ProcessTerminated,
}

/// Errors that can occur when spawning processes.
#[derive(Debug, Error)]
pub enum SpawnError {
    /// Failed to create the process task.
    #[error("failed to create process task: {0}")]
    TaskCreationFailed(String),

    /// Process initialization failed.
    #[error("process initialization failed: {0}")]
    InitFailed(String),
}
