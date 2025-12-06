//! Message trait for typed, self-describing messages.
//!
//! The `Message` trait provides a unified interface for encoding/decoding messages
//! with automatic type tagging. This solves the problem of dispatching to typed
//! handlers when receiving raw bytes.
//!
//! # Encoding Formats
//!
//! - **Local**: Fast encoding using postcard with a tag prefix. Optimized for
//!   same-node communication.
//! - **Remote**: ETF (Erlang Term Format) encoding for BEAM compatibility.
//!   Messages are encoded as `{:tag, ...fields}` tuples.
//!
//! # Example
//!
//! ```ignore
//! use ambitious::message::Message;
//!
//! #[derive(Message)]
//! struct Get;
//!
//! #[derive(Message)]
//! struct Add(i64);
//!
//! // Local encoding (fast, postcard-based)
//! let bytes = Get.encode_local();
//! let (tag, payload) = Message::decode_tag(&bytes);
//! assert_eq!(tag, "Get");
//!
//! // Remote encoding (ETF, BEAM-compatible)
//! let etf_bytes = Add(42).encode_remote();
//! ```

use crate::core::DecodeError;
use serde::{Serialize, de::DeserializeOwned};

/// Encode a value using postcard.
///
/// Helper function for derive macro.
pub fn encode_payload<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("failed to encode payload")
}

/// Decode a value using postcard.
///
/// Helper function for derive macro.
pub fn decode_payload<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    postcard::from_bytes(bytes).map_err(|e| DecodeError::Postcard(e.to_string()))
}

/// Trait for typed, self-describing messages.
///
/// Messages can be encoded in two formats:
/// - Local: postcard with tag prefix (fast, for same-node)
/// - Remote: ETF tuples (for BEAM interop)
///
/// Use `#[derive(Message)]` to automatically implement this trait.
pub trait Message: Sized + Send + 'static {
    /// The unique tag identifying this message type.
    ///
    /// By default, this is the type name (e.g., "Get", "Add").
    /// Can be customized with `#[message(tag = "custom")]`.
    fn tag() -> &'static str;

    /// Encode for local (same-node) communication.
    ///
    /// Format: `[tag_len: u16][tag: &str][payload: postcard]`
    fn encode_local(&self) -> Vec<u8>;

    /// Decode from local format (payload only, tag already stripped).
    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError>;

    /// Encode for remote (cross-node) communication using ETF.
    ///
    /// Format: `{:tag, field1, field2, ...}` as ETF binary.
    fn encode_remote(&self) -> Vec<u8>;

    /// Decode from remote ETF format.
    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError>;
}

/// Extract the tag and payload from a locally-encoded message.
///
/// Returns `(tag, payload)` where payload can be passed to `Message::decode_local`.
pub fn decode_tag(bytes: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
    if bytes.len() < 2 {
        return Err(DecodeError::InvalidData("message too short".into()));
    }

    let tag_len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;

    if bytes.len() < 2 + tag_len {
        return Err(DecodeError::InvalidData(
            "tag length exceeds message".into(),
        ));
    }

    let tag = std::str::from_utf8(&bytes[2..2 + tag_len])
        .map_err(|_| DecodeError::InvalidData("invalid utf8 in tag".into()))?;

    let payload = &bytes[2 + tag_len..];

    Ok((tag, payload))
}

/// Implementation of Message for unit type `()`.
///
/// This is used for acknowledgment messages (like stop replies).
impl Message for () {
    fn tag() -> &'static str {
        "unit"
    }

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &[])
    }

    fn decode_local(_bytes: &[u8]) -> Result<Self, DecodeError> {
        Ok(())
    }

    fn encode_remote(&self) -> Vec<u8> {
        self.encode_local()
    }

    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_local(bytes)
    }
}

/// Implementation of Message for primitive integer types.
macro_rules! impl_message_for_primitive {
    ($t:ty, $tag:expr) => {
        impl Message for $t {
            fn tag() -> &'static str {
                $tag
            }

            fn encode_local(&self) -> Vec<u8> {
                encode_with_tag(Self::tag(), &encode_payload(self))
            }

            fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
                decode_payload(bytes)
            }

            fn encode_remote(&self) -> Vec<u8> {
                self.encode_local()
            }

            fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
                Self::decode_local(bytes)
            }
        }
    };
}

impl_message_for_primitive!(i8, "i8");
impl_message_for_primitive!(i16, "i16");
impl_message_for_primitive!(i32, "i32");
impl_message_for_primitive!(i64, "i64");
impl_message_for_primitive!(u8, "u8");
impl_message_for_primitive!(u16, "u16");
impl_message_for_primitive!(u32, "u32");
impl_message_for_primitive!(u64, "u64");
impl_message_for_primitive!(f32, "f32");
impl_message_for_primitive!(f64, "f64");
impl_message_for_primitive!(bool, "bool");
impl_message_for_primitive!(String, "String");

/// Encode a tag and payload into local format.
///
/// Helper for implementing `encode_local`.
pub fn encode_with_tag(tag: &str, payload: &[u8]) -> Vec<u8> {
    let tag_bytes = tag.as_bytes();
    let tag_len = tag_bytes.len() as u16;

    let mut bytes = Vec::with_capacity(2 + tag_bytes.len() + payload.len());
    bytes.extend_from_slice(&tag_len.to_le_bytes());
    bytes.extend_from_slice(tag_bytes);
    bytes.extend_from_slice(payload);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    // Manual implementation for testing
    struct TestMsg {
        value: i32,
    }

    impl Message for TestMsg {
        fn tag() -> &'static str {
            "TestMsg"
        }

        fn encode_local(&self) -> Vec<u8> {
            let payload = postcard::to_allocvec(&self.value).unwrap();
            encode_with_tag(Self::tag(), &payload)
        }

        fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
            let value: i32 =
                postcard::from_bytes(bytes).map_err(|e| DecodeError::Postcard(e.to_string()))?;
            Ok(TestMsg { value })
        }

        fn encode_remote(&self) -> Vec<u8> {
            // Simplified: just use local encoding for now
            // Real impl would use ETF
            self.encode_local()
        }

        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            // Simplified: just use local decoding for now
            Self::decode_local(bytes)
        }
    }

    #[test]
    fn test_encode_decode_local() {
        let msg = TestMsg { value: 42 };
        let bytes = msg.encode_local();

        let (tag, payload) = decode_tag(&bytes).unwrap();
        assert_eq!(tag, "TestMsg");

        let decoded = TestMsg::decode_local(payload).unwrap();
        assert_eq!(decoded.value, 42);
    }

    #[test]
    fn test_decode_tag_errors() {
        // Too short
        assert!(decode_tag(&[]).is_err());
        assert!(decode_tag(&[0]).is_err());

        // Tag length exceeds message
        assert!(decode_tag(&[10, 0]).is_err());
    }

    // Unit struct test
    struct UnitMsg;

    impl Message for UnitMsg {
        fn tag() -> &'static str {
            "UnitMsg"
        }

        fn encode_local(&self) -> Vec<u8> {
            encode_with_tag(Self::tag(), &[])
        }

        fn decode_local(_bytes: &[u8]) -> Result<Self, DecodeError> {
            Ok(UnitMsg)
        }

        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }

        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    #[test]
    fn test_unit_message() {
        let msg = UnitMsg;
        let bytes = msg.encode_local();

        let (tag, payload) = decode_tag(&bytes).unwrap();
        assert_eq!(tag, "UnitMsg");
        assert!(payload.is_empty());

        let _decoded = UnitMsg::decode_local(payload).unwrap();
    }

    // Note: Derive macro tests are in crates/ambitious-macros/tests/derive_test.rs
    // because the macro generates ::ambitious:: paths which don't work inside the crate
}
