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
use erltf::{OwnedTerm, erl_atom, erl_tuple};
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
    const TAG: &'static str;

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
    const TAG: &'static str = "unit";

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::TAG, &[])
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
            const TAG: &'static str = $tag;

            fn encode_local(&self) -> Vec<u8> {
                encode_with_tag(Self::TAG, &encode_payload(self))
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

/// Implementation of Message for `Option<T>` where T: Message + Serialize + DeserializeOwned.
impl<T: Message + Serialize + DeserializeOwned> Message for Option<T> {
    const TAG: &'static str = "Option";

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::TAG, &encode_payload(self))
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

// =============================================================================
// ETF Encoding Helpers (for remote/BEAM communication)
// =============================================================================

/// Encode a unit type (no payload) as ETF.
///
/// Produces a single atom: `:tag`
///
/// Helper for implementing `encode_remote` for unit structs.
pub fn encode_etf_unit(tag: &str) -> Vec<u8> {
    let term = erl_atom!(tag);
    erltf::encode(&term).expect("failed to encode ETF atom")
}

/// Encode a value as a tagged ETF tuple.
///
/// Produces: `{:tag, payload}` where payload is the serde-serialized value.
///
/// Helper for implementing `encode_remote`.
pub fn encode_etf_with_tag<T: Serialize>(tag: &str, value: &T) -> Vec<u8> {
    // Serialize the value to an ETF term using erltf_serde
    let payload_term = erltf_serde::to_term(value).expect("failed to serialize to ETF term");
    let term = erl_tuple![erl_atom!(tag), payload_term];
    erltf::encode(&term).expect("failed to encode ETF tuple")
}

/// Decode from ETF bytes, expecting a unit atom (for unit structs).
///
/// Expects: `:tag` atom
///
/// Helper for implementing `decode_remote` for unit structs.
pub fn decode_etf_unit(bytes: &[u8], expected_tag: &str) -> Result<(), DecodeError> {
    let term = erltf::decode(bytes).map_err(|e| DecodeError::Etf(e.to_string()))?;

    match term {
        OwnedTerm::Atom(atom) if atom.as_str() == expected_tag => Ok(()),
        OwnedTerm::Tuple(elems) if elems.len() == 1 => {
            // Also accept `{:tag}` tuple form
            if let OwnedTerm::Atom(ref atom) = elems[0]
                && atom.as_str() == expected_tag
            {
                return Ok(());
            }
            Err(DecodeError::Etf(format!(
                "expected atom '{}', got {:?}",
                expected_tag, elems
            )))
        }
        other => Err(DecodeError::Etf(format!(
            "expected atom '{}', got {:?}",
            expected_tag, other
        ))),
    }
}

/// Decode from ETF bytes, expecting a tagged tuple.
///
/// Expects: `{:tag, payload}` and deserializes payload to type T.
///
/// Helper for implementing `decode_remote`.
pub fn decode_etf_with_tag<T: DeserializeOwned>(
    bytes: &[u8],
    expected_tag: &str,
) -> Result<T, DecodeError> {
    let term = erltf::decode(bytes).map_err(|e| DecodeError::Etf(e.to_string()))?;

    match term {
        OwnedTerm::Tuple(elems) if elems.len() == 2 => {
            // Check tag
            if let OwnedTerm::Atom(ref atom) = elems[0] {
                if atom.as_str() != expected_tag {
                    return Err(DecodeError::Etf(format!(
                        "expected tag '{}', got '{}'",
                        expected_tag,
                        atom.as_str()
                    )));
                }
            } else {
                return Err(DecodeError::Etf(format!(
                    "expected atom tag, got {:?}",
                    elems[0]
                )));
            }

            // Deserialize payload
            erltf_serde::from_term(&elems[1])
                .map_err(|e| DecodeError::Etf(format!("failed to deserialize payload: {}", e)))
        }
        other => Err(DecodeError::Etf(format!(
            "expected tagged tuple {{:{}, _}}, got {:?}",
            expected_tag, other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Manual implementation for testing
    struct TestMsg {
        value: i32,
    }

    impl Message for TestMsg {
        const TAG: &'static str = "TestMsg";

        fn encode_local(&self) -> Vec<u8> {
            let payload = postcard::to_allocvec(&self.value).unwrap();
            encode_with_tag(Self::TAG, &payload)
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
        const TAG: &'static str = "UnitMsg";

        fn encode_local(&self) -> Vec<u8> {
            encode_with_tag(Self::TAG, &[])
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

    // =============================================================================
    // ETF Encoding Tests
    // =============================================================================

    #[test]
    fn test_encode_etf_unit() {
        let bytes = encode_etf_unit("ping");
        // Decode and verify it's an atom
        let term = erltf::decode(&bytes).unwrap();
        match term {
            OwnedTerm::Atom(atom) => assert_eq!(atom.as_str(), "ping"),
            _ => panic!("expected atom, got {:?}", term),
        }
    }

    #[test]
    fn test_decode_etf_unit() {
        // Encode an atom
        let term = erl_atom!("pong");
        let bytes = erltf::encode(&term).unwrap();

        // Decode it
        decode_etf_unit(&bytes, "pong").unwrap();

        // Wrong tag should fail
        assert!(decode_etf_unit(&bytes, "ping").is_err());
    }

    #[test]
    fn test_encode_etf_with_tag() {
        use serde::Serialize;

        #[derive(Serialize)]
        struct TestPayload {
            value: i32,
        }

        let payload = TestPayload { value: 42 };
        let bytes = encode_etf_with_tag("test", &payload);

        // Decode and verify structure
        let term = erltf::decode(&bytes).unwrap();
        match term {
            OwnedTerm::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                match &elems[0] {
                    OwnedTerm::Atom(atom) => assert_eq!(atom.as_str(), "test"),
                    _ => panic!("expected atom tag"),
                }
            }
            _ => panic!("expected tuple"),
        }
    }

    #[test]
    fn test_decode_etf_with_tag() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct TestPayload {
            value: i32,
        }

        // Encode
        let original = TestPayload { value: 123 };
        let bytes = encode_etf_with_tag("test", &original);

        // Decode
        let decoded: TestPayload = decode_etf_with_tag(&bytes, "test").unwrap();
        assert_eq!(decoded, original);

        // Wrong tag should fail
        assert!(decode_etf_with_tag::<TestPayload>(&bytes, "wrong").is_err());
    }

    #[test]
    fn test_etf_roundtrip_primitive() {
        // Test with primitive types
        let value: i64 = 12345;
        let bytes = encode_etf_with_tag("num", &value);
        let decoded: i64 = decode_etf_with_tag(&bytes, "num").unwrap();
        assert_eq!(decoded, value);

        let value = "hello world".to_string();
        let bytes = encode_etf_with_tag("str", &value);
        let decoded: String = decode_etf_with_tag(&bytes, "str").unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn test_etf_roundtrip_complex() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Complex {
            name: String,
            values: Vec<i32>,
            nested: Option<Box<Complex>>,
        }

        let original = Complex {
            name: "root".to_string(),
            values: vec![1, 2, 3],
            nested: Some(Box::new(Complex {
                name: "child".to_string(),
                values: vec![4, 5],
                nested: None,
            })),
        };

        let bytes = encode_etf_with_tag("complex", &original);
        let decoded: Complex = decode_etf_with_tag(&bytes, "complex").unwrap();
        assert_eq!(decoded, original);
    }
}
