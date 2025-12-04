//! GenFsm protocol messages.

use crate::core::{Pid, Ref};
use serde::{Deserialize, Serialize};

/// Messages sent to a GenFsm process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FsmMessage {
    /// An event to be processed.
    Event(Vec<u8>),
    /// A synchronous call request.
    Call {
        /// The calling process.
        from: Pid,
        /// Unique reference for this call.
        reference: Ref,
        /// The encoded call request.
        request: Vec<u8>,
    },
    /// A reply to a pending call.
    Reply {
        /// The reference of the original call.
        reference: Ref,
        /// The encoded reply.
        reply: Vec<u8>,
    },
    /// State timeout fired.
    StateTimeout,
    /// Generic timeout fired.
    GenericTimeout(String),
}
