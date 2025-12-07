//! GenServer internal protocol messages.
//!
//! Protocol messages wrap typed payloads for call/cast/reply coordination.

use crate::core::{DecodeError, ExitReason, Ref, Term};
use serde::{Deserialize, Serialize};

/// Reference to a caller awaiting a reply.
///
/// Used in `Call::call` to identify who to reply to.
/// Can be used with `reply()` for delayed responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct From {
    /// The caller's process ID.
    pub pid: crate::core::Pid,
    /// Unique reference for this call.
    pub reference: Ref,
}

/// Internal GenServer protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// A synchronous call request.
    Call {
        /// The From handle for replying.
        from: From,
        /// The encoded request payload.
        payload: Vec<u8>,
    },
    /// An asynchronous cast message.
    Cast {
        /// The encoded message payload.
        payload: Vec<u8>,
    },
    /// A reply to a call.
    Reply {
        /// The reference matching the original call.
        reference: Ref,
        /// The encoded reply payload.
        payload: Vec<u8>,
    },
    /// A stop request.
    Stop {
        /// The reason to stop.
        reason: ExitReason,
        /// The From handle for replying (if stopping via call).
        from: Option<From>,
    },
    /// Internal timeout message.
    Timeout,
    /// Internal continue message.
    Continue {
        /// The encoded continue argument.
        arg: Vec<u8>,
    },
}

impl Message {
    /// Encode this message to bytes.
    pub fn encode(&self) -> Vec<u8> {
        Term::encode(self)
    }

    /// Decode a message from bytes.
    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        Term::decode(data)
    }
}

/// Encode a call request.
#[allow(dead_code)]
pub fn encode_call<M: Term>(from: From, request: &M) -> Vec<u8> {
    Message::Call {
        from,
        payload: request.encode(),
    }
    .encode()
}

/// Encode a cast message.
#[allow(dead_code)]
pub fn encode_cast<M: Term>(msg: &M) -> Vec<u8> {
    Message::Cast {
        payload: msg.encode(),
    }
    .encode()
}

/// Encode a reply from raw bytes.
///
/// The payload should already be encoded (e.g., via `Message::encode_local()`).
pub fn encode_reply(reference: Ref, payload: &[u8]) -> Vec<u8> {
    Message::Reply {
        reference,
        payload: payload.to_vec(),
    }
    .encode()
}

/// Encode a stop request.
pub fn encode_stop(reason: ExitReason, from: Option<From>) -> Vec<u8> {
    Message::Stop { reason, from }.encode()
}

/// Encode a timeout message.
pub fn encode_timeout() -> Vec<u8> {
    Message::Timeout.encode()
}

/// Encode a continue message.
pub fn encode_continue(arg: &[u8]) -> Vec<u8> {
    Message::Continue { arg: arg.to_vec() }.encode()
}
