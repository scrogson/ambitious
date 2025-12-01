//! Internal protocol messages for GenServer communication.
//!
//! These messages are used internally for call/cast/reply coordination.

use crate::types::From;
use dream_core::{DecodeError, ExitReason, Message, Ref};
use serde::{Deserialize, Serialize};

/// Internal GenServer protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GenServerMessage {
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

// GenServerMessage already implements Message via the blanket impl

/// Encodes a call request.
pub fn encode_call<M: Message>(from: From, request: &M) -> Vec<u8> {
    let msg = GenServerMessage::Call {
        from,
        payload: request.encode(),
    };
    msg.encode()
}

/// Encodes a cast message.
pub fn encode_cast<M: Message>(msg: &M) -> Vec<u8> {
    let msg = GenServerMessage::Cast {
        payload: msg.encode(),
    };
    msg.encode()
}

/// Encodes a reply.
pub fn encode_reply<M: Message>(reference: Ref, reply: &M) -> Vec<u8> {
    let msg = GenServerMessage::Reply {
        reference,
        payload: reply.encode(),
    };
    msg.encode()
}

/// Encodes a stop request.
pub fn encode_stop(reason: ExitReason, from: Option<From>) -> Vec<u8> {
    let msg = GenServerMessage::Stop { reason, from };
    msg.encode()
}

/// Encodes a timeout message.
pub fn encode_timeout() -> Vec<u8> {
    GenServerMessage::Timeout.encode()
}

/// Encodes a continue message.
pub fn encode_continue(arg: &[u8]) -> Vec<u8> {
    let msg = GenServerMessage::Continue { arg: arg.to_vec() };
    msg.encode()
}

/// Decodes a GenServer protocol message.
pub fn decode(data: &[u8]) -> Result<GenServerMessage, DecodeError> {
    <GenServerMessage as Message>::decode(data)
}
