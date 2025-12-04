//! GenFsm error types.

use crate::core::{ExitReason, Pid};
use thiserror::Error;

/// Error returned when starting a GenFsm fails.
#[derive(Debug, Error)]
pub enum StartError {
    /// The init callback returned `InitResult::Stop`.
    #[error("init stopped: {0}")]
    Stop(ExitReason),
    /// The init callback returned `InitResult::Ignore`.
    #[error("init ignored")]
    Ignore,
    /// Failed to spawn the process.
    #[error("spawn failed")]
    SpawnFailed,
}

/// Error returned when a call fails.
#[derive(Debug, Error)]
pub enum CallError {
    /// The FSM process is not running.
    #[error("process not found: {0:?}")]
    ProcessNotFound(Pid),
    /// The call timed out.
    #[error("timeout")]
    Timeout,
    /// The FSM stopped while processing the call.
    #[error("fsm stopped: {0}")]
    Stopped(ExitReason),
    /// Failed to decode the reply.
    #[error("decode error: {0}")]
    DecodeError(String),
    /// The FSM was not in expected state.
    #[error("invalid state")]
    InvalidState,
}

/// Error returned when sending an event fails.
#[derive(Debug, Error)]
pub enum SendEventError {
    /// The FSM process is not running.
    #[error("process not found: {0:?}")]
    ProcessNotFound(Pid),
    /// Failed to encode the event.
    #[error("encode error")]
    EncodeError,
}
