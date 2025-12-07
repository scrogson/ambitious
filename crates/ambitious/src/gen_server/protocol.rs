//! GenServer internal protocol messages.
//!
//! Protocol messages wrap typed payloads for call/cast/reply coordination.
//!
//! ## OTP Envelope Format
//!
//! For BEAM compatibility, GenServer messages are wrapped in OTP-style envelopes:
//! - **Call**: `{'$gen_call', {Pid, Ref}, Request}` - synchronous request/response
//! - **Cast**: `{'$gen_cast', Request}` - asynchronous fire-and-forget
//!
//! Messages not wrapped in an envelope are treated as "info" messages.

use crate::core::{DecodeError, ExitReason, Ref, Term};
use serde::{Deserialize, Serialize};

// =============================================================================
// OTP Envelope Tags
// =============================================================================

/// Tag for GenServer call envelope (matches Erlang's `$gen_call`).
pub const GEN_CALL_TAG: &str = "$gen_call";

/// Tag for GenServer cast envelope (matches Erlang's `$gen_cast`).
pub const GEN_CAST_TAG: &str = "$gen_cast";

// =============================================================================
// OTP Envelope Types
// =============================================================================

/// OTP-style GenServer call envelope.
///
/// Format: `{'$gen_call', {Pid, Ref}, Request}`
///
/// This envelope wraps a synchronous call request. The `from` field identifies
/// the caller for sending the reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenCallEnvelope {
    /// The caller information for sending the reply.
    pub from: From,
    /// The encoded request payload.
    pub request: Vec<u8>,
}

/// OTP-style GenServer cast envelope.
///
/// Format: `{'$gen_cast', Request}`
///
/// This envelope wraps an asynchronous cast message. There is no reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenCastEnvelope {
    /// The encoded message payload.
    pub request: Vec<u8>,
}

/// A tagged envelope for behavior-agnostic message routing.
///
/// This is the wire format for all behavior messages. The tag identifies
/// which behavior should handle the message (e.g., `$gen_call`, `$gen_cast`,
/// `$gen_stage`, `$channel`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedEnvelope {
    /// The behavior tag (e.g., `$gen_call`, `$gen_cast`).
    pub tag: String,
    /// The envelope payload (behavior-specific format).
    pub payload: Vec<u8>,
}

impl TaggedEnvelope {
    /// Create a new tagged envelope.
    pub fn new(tag: &str, payload: Vec<u8>) -> Self {
        Self {
            tag: tag.to_string(),
            payload,
        }
    }

    /// Encode this envelope to bytes.
    pub fn encode(&self) -> Vec<u8> {
        Term::encode(self)
    }

    /// Try to decode a tagged envelope from bytes.
    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        Term::decode(data)
    }
}

// =============================================================================
// Envelope Encoding/Decoding
// =============================================================================

/// Encode a GenServer call envelope with OTP-compatible format.
///
/// The message is wrapped in a `$gen_call` tagged envelope.
pub fn encode_gen_call_envelope(from: From, request: Vec<u8>) -> Vec<u8> {
    let envelope = GenCallEnvelope { from, request };
    let envelope_bytes = Term::encode(&envelope);
    TaggedEnvelope::new(GEN_CALL_TAG, envelope_bytes).encode()
}

/// Encode a GenServer cast envelope with OTP-compatible format.
///
/// The message is wrapped in a `$gen_cast` tagged envelope.
pub fn encode_gen_cast_envelope(request: Vec<u8>) -> Vec<u8> {
    let envelope = GenCastEnvelope { request };
    let envelope_bytes = Term::encode(&envelope);
    TaggedEnvelope::new(GEN_CAST_TAG, envelope_bytes).encode()
}

/// Try to decode a `$gen_call` envelope from bytes.
///
/// Returns `Some(GenCallEnvelope)` if the bytes represent a valid call envelope,
/// or `None` if it's not a call envelope (might be cast, info, or other).
pub fn decode_gen_call_envelope(data: &[u8]) -> Option<GenCallEnvelope> {
    let tagged = TaggedEnvelope::decode(data).ok()?;
    if tagged.tag != GEN_CALL_TAG {
        return None;
    }
    Term::decode(&tagged.payload).ok()
}

/// Try to decode a `$gen_cast` envelope from bytes.
///
/// Returns `Some(GenCastEnvelope)` if the bytes represent a valid cast envelope,
/// or `None` if it's not a cast envelope (might be call, info, or other).
pub fn decode_gen_cast_envelope(data: &[u8]) -> Option<GenCastEnvelope> {
    let tagged = TaggedEnvelope::decode(data).ok()?;
    if tagged.tag != GEN_CAST_TAG {
        return None;
    }
    Term::decode(&tagged.payload).ok()
}

/// Try to decode any tagged envelope from bytes.
///
/// Returns the tag and payload if successful. Use this for custom behaviors
/// that define their own envelope tags.
#[allow(dead_code)]
pub fn decode_tagged_envelope(data: &[u8]) -> Option<(String, Vec<u8>)> {
    let tagged = TaggedEnvelope::decode(data).ok()?;
    Some((tagged.tag, tagged.payload))
}

// =============================================================================
// Legacy Types (kept for compatibility during migration)
// =============================================================================

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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Pid;

    #[test]
    fn test_gen_call_envelope_roundtrip() {
        let from = From {
            pid: Pid::new(),
            reference: Ref::new(),
        };
        let request = b"hello".to_vec();

        let encoded = encode_gen_call_envelope(from.clone(), request.clone());
        let decoded = decode_gen_call_envelope(&encoded).expect("should decode");

        assert_eq!(decoded.from.pid, from.pid);
        assert_eq!(decoded.request, request);
    }

    #[test]
    fn test_gen_cast_envelope_roundtrip() {
        let request = b"cast message".to_vec();

        let encoded = encode_gen_cast_envelope(request.clone());
        let decoded = decode_gen_cast_envelope(&encoded).expect("should decode");

        assert_eq!(decoded.request, request);
    }

    #[test]
    fn test_decode_wrong_envelope_type() {
        // Encode a cast envelope
        let cast_bytes = encode_gen_cast_envelope(b"test".to_vec());

        // Try to decode as call - should return None
        assert!(decode_gen_call_envelope(&cast_bytes).is_none());

        // Encode a call envelope
        let from = From {
            pid: Pid::new(),
            reference: Ref::new(),
        };
        let call_bytes = encode_gen_call_envelope(from, b"test".to_vec());

        // Try to decode as cast - should return None
        assert!(decode_gen_cast_envelope(&call_bytes).is_none());
    }

    #[test]
    fn test_decode_tagged_envelope() {
        let from = From {
            pid: Pid::new(),
            reference: Ref::new(),
        };
        let call_bytes = encode_gen_call_envelope(from, b"test".to_vec());

        let (tag, _payload) = decode_tagged_envelope(&call_bytes).expect("should decode");
        assert_eq!(tag, GEN_CALL_TAG);
    }

    #[test]
    fn test_non_envelope_returns_none() {
        // Random bytes that aren't an envelope
        let random_bytes = b"not an envelope";

        assert!(decode_gen_call_envelope(random_bytes).is_none());
        assert!(decode_gen_cast_envelope(random_bytes).is_none());
        assert!(decode_tagged_envelope(random_bytes).is_none());
    }
}
