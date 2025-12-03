//! Term serialization trait.
//!
//! The [`Term`] trait provides a common interface for encoding and decoding
//! Erlang-like terms. Any type that implements `Serialize + DeserializeOwned`
//! can be used as a Term, similar to how Erlang treats all values as terms.
//!
//! Terms are used throughout DREAM for:
//! - Messages sent between processes
//! - Registry keys
//! - GenServer calls, casts, and replies
//! - Any data that needs to be serialized
//!
//! Uses `postcard` for compact binary serialization.

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

/// Error type for term decoding failures.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// Failed to deserialize the term bytes.
    #[error("failed to decode term: {0}")]
    Deserialize(#[from] postcard::Error),
}

/// A trait for Erlang-like terms that can be serialized and sent between processes.
///
/// This trait is automatically implemented for any type that implements
/// `Serialize + DeserializeOwned + Send + 'static`. The serialization uses
/// `postcard` for compact, efficient binary encoding.
///
/// In Erlang/Elixir, everything is a "term" - atoms, tuples, lists, maps, etc.
/// In DREAM, any serializable Rust type can be a Term, giving you the same
/// flexibility with Rust's type safety.
///
/// # Examples
///
/// ```
/// use dream_core::Term;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// struct Ping {
///     id: u32,
/// }
///
/// let ping = Ping { id: 42 };
/// let bytes = ping.encode();
/// let decoded = Ping::decode(&bytes).unwrap();
/// assert_eq!(ping, decoded);
/// ```
///
/// Terms can be any serializable type:
///
/// ```
/// use dream_core::Term;
///
/// // Primitives
/// let n: u64 = 42;
/// assert_eq!(u64::decode(&n.encode()).unwrap(), 42);
///
/// // Strings
/// let s = "hello".to_string();
/// assert_eq!(String::decode(&s.encode()).unwrap(), "hello");
///
/// // Tuples (like Erlang tuples)
/// let t = ("room".to_string(), "lobby".to_string(), 123u32);
/// let decoded: (String, String, u32) = Term::decode(&t.encode()).unwrap();
/// assert_eq!(t, decoded);
/// ```
pub trait Term: Sized + Send + 'static {
    /// Encodes this term into bytes.
    ///
    /// # Panics
    ///
    /// Panics if serialization fails. This should not happen for well-formed
    /// types that properly implement `Serialize`.
    fn encode(&self) -> Vec<u8>;

    /// Decodes a term from bytes.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError` if the bytes cannot be deserialized into this type.
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError>;

    /// Encodes this term, returning `None` on failure instead of panicking.
    fn try_encode(&self) -> Option<Vec<u8>>;
}

impl<T> Term for T
where
    T: Serialize + DeserializeOwned + Send + 'static,
{
    fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("term serialization failed")
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        postcard::from_bytes(bytes).map_err(DecodeError::from)
    }

    fn try_encode(&self) -> Option<Vec<u8>> {
        postcard::to_allocvec(self).ok()
    }
}

/// Type alias for backward compatibility.
///
/// `Message` is now called `Term` to better reflect its Erlang-like nature.
/// This alias allows existing code to continue working.
#[deprecated(since = "0.2.0", note = "Use `Term` instead of `Message`")]
pub trait Message: Term {}

#[allow(deprecated)]
impl<T: Term> Message for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestTerm {
        id: u32,
        name: String,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    enum TestEnum {
        Ping(u64),
        Pong { value: String },
    }

    #[test]
    fn test_encode_decode_struct() {
        let term = TestTerm {
            id: 42,
            name: "hello".to_string(),
        };
        let bytes = term.encode();
        let decoded = TestTerm::decode(&bytes).unwrap();
        assert_eq!(term, decoded);
    }

    #[test]
    fn test_encode_decode_enum() {
        let term = TestEnum::Ping(123);
        let bytes = term.encode();
        let decoded = TestEnum::decode(&bytes).unwrap();
        assert_eq!(term, decoded);

        let term = TestEnum::Pong {
            value: "world".to_string(),
        };
        let bytes = term.encode();
        let decoded = TestEnum::decode(&bytes).unwrap();
        assert_eq!(term, decoded);
    }

    #[test]
    fn test_decode_error() {
        let bad_bytes = vec![0xFF, 0xFF, 0xFF];
        let result = TestTerm::decode(&bad_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_try_encode() {
        let term = TestTerm {
            id: 1,
            name: "test".to_string(),
        };
        let bytes = term.try_encode();
        assert!(bytes.is_some());
    }

    #[test]
    fn test_primitive_types() {
        // u32
        let num: u32 = 42;
        let bytes = num.encode();
        let decoded = u32::decode(&bytes).unwrap();
        assert_eq!(num, decoded);

        // String
        let s = "hello world".to_string();
        let bytes = s.encode();
        let decoded = String::decode(&bytes).unwrap();
        assert_eq!(s, decoded);

        // Vec<u8>
        let v: Vec<u8> = vec![1, 2, 3, 4];
        let bytes = v.encode();
        let decoded = Vec::<u8>::decode(&bytes).unwrap();
        assert_eq!(v, decoded);
    }

    #[test]
    fn test_option_types() {
        let some: Option<u32> = Some(42);
        let bytes = some.encode();
        let decoded = Option::<u32>::decode(&bytes).unwrap();
        assert_eq!(some, decoded);

        let none: Option<u32> = None;
        let bytes = none.encode();
        let decoded = Option::<u32>::decode(&bytes).unwrap();
        assert_eq!(none, decoded);
    }

    #[test]
    fn test_tuple_types() {
        // Tuples work like Erlang tuples - great for registry keys!
        let tuple: (u32, String, bool) = (42, "test".to_string(), true);
        let bytes = tuple.encode();
        let decoded = <(u32, String, bool)>::decode(&bytes).unwrap();
        assert_eq!(tuple, decoded);
    }

    #[test]
    fn test_unit_type() {
        let unit: () = ();
        let bytes = unit.encode();
        let decoded = <()>::decode(&bytes).unwrap();
        assert_eq!(unit, decoded);
    }

    #[test]
    fn test_tuple_as_registry_key() {
        // This demonstrates using tuples as registry keys like Elixir
        // e.g., {:room, "lobby"} or {:user, user_id}
        let key = ("room".to_string(), "lobby".to_string());
        let bytes = key.encode();
        let decoded: (String, String) = Term::decode(&bytes).unwrap();
        assert_eq!(key, decoded);

        // More complex key
        let key = ("game".to_string(), 123u64, "player1".to_string());
        let bytes = key.encode();
        let decoded: (String, u64, String) = Term::decode(&bytes).unwrap();
        assert_eq!(key, decoded);
    }
}
