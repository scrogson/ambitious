//! Erlang Term Format (ETF) integration.
//!
//! This module provides ETF-based serialization using the `erltf` crate,
//! enabling compatibility with Erlang/Elixir nodes.
//!
//! # Type Discrimination
//!
//! Unlike postcard's compact binary format, ETF is self-describing. Each value
//! carries type information, making it possible to safely decode messages
//! without knowing the type upfront.
//!
//! Messages are conventionally tagged tuples, similar to Erlang:
//! ```erlang
//! {presence_delta, #{joins => [...], leaves => [...]}}
//! {channel_info, after_join}
//! ```
//!
//! # BEAM Compatibility
//!
//! Using ETF enables direct communication with BEAM nodes (Erlang, Elixir, Gleam).
//! Ambitious nodes could potentially join existing Erlang clusters.

pub use erltf::{Atom, ExternalPid, ExternalReference};
use erltf::{OwnedTerm, decode, encode};

/// Re-export the OwnedTerm for pattern matching.
pub use erltf::OwnedTerm as ErlTerm;

/// Error type for ETF operations.
#[derive(Debug, thiserror::Error)]
pub enum EtfError {
    /// Failed to decode ETF bytes.
    #[error("ETF decode error: {0}")]
    Decode(#[from] erltf::DecodeError),
    /// Failed to encode to ETF bytes.
    #[error("ETF encode error: {0}")]
    Encode(#[from] erltf::EncodeError),
    /// The term structure didn't match the expected format.
    #[error("Unexpected term format: expected {expected}, got {got}")]
    UnexpectedFormat {
        /// What format was expected.
        expected: &'static str,
        /// What was actually received.
        got: String,
    },
}

/// A wrapper around ETF bytes that provides type-safe decoding.
///
/// Unlike [`RawTerm`](super::RawTerm) which uses postcard, `EtfTerm` uses
/// Erlang's External Term Format, which is self-describing and enables
/// safe runtime type discrimination.
#[derive(Debug, Clone)]
pub struct EtfTerm {
    bytes: Vec<u8>,
}

impl EtfTerm {
    /// Create a new ETF term from raw bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Decode the raw bytes into an OwnedTerm for pattern matching.
    ///
    /// This is the primary way to handle ETF messages - decode to OwnedTerm
    /// and pattern match on the structure:
    ///
    /// ```ignore
    /// match etf_term.decode_term()? {
    ///     OwnedTerm::Tuple(elems) => match elems.as_slice() {
    ///         [OwnedTerm::Atom(tag), payload] if tag.as_str() == "presence_delta" => {
    ///             // handle presence delta
    ///         }
    ///         [OwnedTerm::Atom(tag), ..] if tag.as_str() == "channel_info" => {
    ///             // handle channel info
    ///         }
    ///         _ => { /* unknown message */ }
    ///     }
    ///     _ => { /* not a tuple */ }
    /// }
    /// ```
    pub fn decode_term(&self) -> Result<OwnedTerm, EtfError> {
        decode(&self.bytes).map_err(EtfError::from)
    }

    /// Check if this term is a tagged tuple with the given atom tag.
    ///
    /// Returns the remaining elements if matched.
    pub fn match_tagged(&self, expected_tag: &str) -> Option<Vec<OwnedTerm>> {
        let term = self.decode_term().ok()?;
        match term {
            OwnedTerm::Tuple(mut elems) if !elems.is_empty() => {
                if let OwnedTerm::Atom(ref atom) = elems[0]
                    && atom.as_str() == expected_tag
                {
                    elems.remove(0);
                    return Some(elems);
                }
                None
            }
            _ => None,
        }
    }

    /// Get the tag atom if this is a tagged tuple.
    pub fn tag(&self) -> Option<String> {
        let term = self.decode_term().ok()?;
        match term {
            OwnedTerm::Tuple(elems) if !elems.is_empty() => {
                if let OwnedTerm::Atom(ref atom) = elems[0] {
                    return Some(atom.as_str().to_string());
                }
                None
            }
            _ => None,
        }
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume and return the raw bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl From<Vec<u8>> for EtfTerm {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

impl From<EtfTerm> for Vec<u8> {
    fn from(term: EtfTerm) -> Self {
        term.bytes
    }
}

/// Trait for types that can be converted to/from ETF.
///
/// This provides a bridge between Rust types and ETF representation.
/// Types implementing this trait can be sent to BEAM nodes.
pub trait ToEtf {
    /// Convert this value to an ETF OwnedTerm.
    fn to_etf(&self) -> OwnedTerm;

    /// Encode this value to ETF bytes.
    fn encode_etf(&self) -> Result<Vec<u8>, EtfError> {
        encode(&self.to_etf()).map_err(EtfError::from)
    }
}

/// Trait for types that can be decoded from ETF.
pub trait FromEtf: Sized {
    /// Try to decode from an ETF OwnedTerm.
    fn from_etf(term: OwnedTerm) -> Result<Self, EtfError>;

    /// Decode from ETF bytes.
    fn decode_etf(bytes: &[u8]) -> Result<Self, EtfError> {
        let term = decode(bytes)?;
        Self::from_etf(term)
    }
}

// =============================================================================
// Example implementations showing how message types would work
// =============================================================================

/// Example: A simple ping message as ETF.
///
/// In Erlang: `{ping, SeqNum}`
#[derive(Debug, Clone, PartialEq)]
pub struct EtfPing {
    /// Sequence number for the ping.
    pub seq: u64,
}

impl ToEtf for EtfPing {
    fn to_etf(&self) -> OwnedTerm {
        OwnedTerm::Tuple(vec![
            OwnedTerm::Atom(Atom::from("ping")),
            OwnedTerm::Integer(self.seq as i64),
        ])
    }
}

impl FromEtf for EtfPing {
    fn from_etf(term: OwnedTerm) -> Result<Self, EtfError> {
        match term {
            OwnedTerm::Tuple(elems) => match elems.as_slice() {
                [OwnedTerm::Atom(tag), OwnedTerm::Integer(seq)] if tag.as_str() == "ping" => {
                    Ok(EtfPing { seq: *seq as u64 })
                }
                _ => Err(EtfError::UnexpectedFormat {
                    expected: "{ping, integer()}",
                    got: format!("{:?}", elems),
                }),
            },
            other => Err(EtfError::UnexpectedFormat {
                expected: "tuple",
                got: format!("{:?}", other),
            }),
        }
    }
}

/// Example: A pong response.
///
/// In Erlang: `{pong, SeqNum}`
#[derive(Debug, Clone, PartialEq)]
pub struct EtfPong {
    /// Sequence number for the pong (should match the ping).
    pub seq: u64,
}

impl ToEtf for EtfPong {
    fn to_etf(&self) -> OwnedTerm {
        OwnedTerm::Tuple(vec![
            OwnedTerm::Atom(Atom::from("pong")),
            OwnedTerm::Integer(self.seq as i64),
        ])
    }
}

impl FromEtf for EtfPong {
    fn from_etf(term: OwnedTerm) -> Result<Self, EtfError> {
        match term {
            OwnedTerm::Tuple(elems) => match elems.as_slice() {
                [OwnedTerm::Atom(tag), OwnedTerm::Integer(seq)] if tag.as_str() == "pong" => {
                    Ok(EtfPong { seq: *seq as u64 })
                }
                _ => Err(EtfError::UnexpectedFormat {
                    expected: "{pong, integer()}",
                    got: format!("{:?}", elems),
                }),
            },
            other => Err(EtfError::UnexpectedFormat {
                expected: "tuple",
                got: format!("{:?}", other),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ping_roundtrip() {
        let ping = EtfPing { seq: 42 };
        let bytes = ping.encode_etf().unwrap();
        let decoded = EtfPing::decode_etf(&bytes).unwrap();
        assert_eq!(ping, decoded);
    }

    #[test]
    fn test_pong_roundtrip() {
        let pong = EtfPong { seq: 123 };
        let bytes = pong.encode_etf().unwrap();
        let decoded = EtfPong::decode_etf(&bytes).unwrap();
        assert_eq!(pong, decoded);
    }

    #[test]
    fn test_etf_term_tag() {
        let ping = EtfPing { seq: 1 };
        let bytes = ping.encode_etf().unwrap();
        let term = EtfTerm::new(bytes);
        assert_eq!(term.tag(), Some("ping".to_string()));
    }

    #[test]
    fn test_etf_term_match_tagged() {
        let ping = EtfPing { seq: 99 };
        let bytes = ping.encode_etf().unwrap();
        let term = EtfTerm::new(bytes);

        // Should match "ping"
        let elems = term.match_tagged("ping").unwrap();
        assert_eq!(elems.len(), 1);
        assert!(matches!(elems[0], OwnedTerm::Integer(99)));

        // Should not match "pong"
        assert!(term.match_tagged("pong").is_none());
    }

    #[test]
    fn test_discriminate_message_types() {
        // This demonstrates runtime type discrimination with ETF
        let ping = EtfPing { seq: 1 };
        let pong = EtfPong { seq: 2 };

        let ping_bytes = ping.encode_etf().unwrap();
        let pong_bytes = pong.encode_etf().unwrap();

        // We can determine the type at runtime from the tag
        let term1 = EtfTerm::new(ping_bytes);
        let term2 = EtfTerm::new(pong_bytes);

        assert_eq!(term1.tag(), Some("ping".to_string()));
        assert_eq!(term2.tag(), Some("pong".to_string()));

        // And decode to the appropriate type
        if term1.tag().as_deref() == Some("ping") {
            let decoded = EtfPing::decode_etf(term1.as_bytes()).unwrap();
            assert_eq!(decoded.seq, 1);
        }

        if term2.tag().as_deref() == Some("pong") {
            let decoded = EtfPong::decode_etf(term2.as_bytes()).unwrap();
            assert_eq!(decoded.seq, 2);
        }
    }

    #[test]
    fn test_using_erltf_macros() {
        use erltf::{erl_atom, erl_tuple};

        // Build terms using macros (like Erlang syntax)
        let term = erl_tuple![erl_atom!("hello"), OwnedTerm::Integer(42)];

        let bytes = encode(&term).unwrap();
        let decoded = decode(&bytes).unwrap();

        match decoded {
            OwnedTerm::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0], OwnedTerm::Atom(a) if a.as_str() == "hello"));
                assert!(matches!(elems[1], OwnedTerm::Integer(42)));
            }
            _ => panic!("expected tuple"),
        }
    }
}
