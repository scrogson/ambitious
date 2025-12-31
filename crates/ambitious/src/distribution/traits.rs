//! Trait abstractions for pluggable transports and protocols.
//!
//! These traits allow the distribution layer to work with different
//! transport mechanisms (QUIC, TCP) and wire protocols (postcard, ETF).

use super::protocol::{DistError, DistMessage};
use async_trait::async_trait;
use std::net::SocketAddr;

// =============================================================================
// Transport Traits
// =============================================================================

/// A transport endpoint that can accept incoming connections and make outgoing ones.
///
/// Implementations include `QuicTransport` (for Rust-to-Rust) and
/// `TcpTransport` (for Erlang distribution protocol compatibility).
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// The connection type produced by this transport.
    type Connection: TransportConnection;

    /// Accept an incoming connection.
    ///
    /// Returns `None` if the transport is shutting down.
    async fn accept(&self) -> Option<Self::Connection>;

    /// Connect to a remote node.
    async fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<Self::Connection, DistError>;

    /// Get the local address this transport is bound to.
    fn local_addr(&self) -> Result<SocketAddr, DistError>;

    /// Get our node name.
    fn node_name(&self) -> &str;

    /// Get our creation number.
    fn creation(&self) -> u32;

    /// Close the transport endpoint.
    fn close(&self);
}

/// A connection to a remote node.
///
/// Provides bidirectional message passing over the underlying transport.
#[async_trait]
pub trait TransportConnection: Send + Sync + Clone + 'static {
    /// The stream type for receiving data.
    type RecvStream: RecvStream;

    /// Send a message on this connection.
    ///
    /// The message is encoded and framed according to the transport's protocol.
    async fn send_message(&self, msg: &DistMessage) -> Result<(), DistError>;

    /// Accept an incoming stream and receive a message.
    ///
    /// Returns the received message and the stream for potential further reading.
    async fn accept_and_recv(&self) -> Result<(DistMessage, Self::RecvStream), DistError>;

    /// Get the remote address.
    fn remote_address(&self) -> SocketAddr;

    /// Close this connection.
    fn close(&self, reason: &str);

    /// Check if the connection is still open.
    fn is_open(&self) -> bool;
}

/// A receive stream for reading additional data after the initial message.
#[allow(dead_code)]
#[async_trait]
pub trait RecvStream: Send + 'static {
    /// Receive a message from this stream.
    async fn recv_message(&mut self) -> Result<DistMessage, DistError>;
}

// =============================================================================
// Protocol Traits
// =============================================================================

/// Protocol for encoding/decoding distribution messages.
///
/// Implementations include `PostcardProtocol` (default, fast) and
/// `ErlangProtocol` (for BEAM node compatibility).
pub trait Protocol: Send + Sync + 'static {
    /// Encode a distribution message to bytes.
    fn encode(&self, msg: &DistMessage) -> Result<Vec<u8>, DistError>;

    /// Decode a distribution message from bytes.
    fn decode(&self, bytes: &[u8]) -> Result<DistMessage, DistError>;

    /// Frame an encoded message with length prefix.
    fn frame(&self, payload: Vec<u8>) -> Vec<u8>;

    /// Try to parse a framed message from a buffer.
    ///
    /// Returns `Some((message, bytes_consumed))` if complete, `None` if more data needed.
    fn parse_frame(&self, buf: &[u8]) -> Result<Option<(DistMessage, usize)>, DistError>;
}

// =============================================================================
// Default Protocol Implementation (Postcard)
// =============================================================================

/// Default protocol using postcard serialization.
///
/// This is the native Ambitious protocol - fast and compact, but not
/// compatible with Erlang/BEAM nodes.
#[derive(Debug, Clone, Default)]
pub struct PostcardProtocol;

impl Protocol for PostcardProtocol {
    fn encode(&self, msg: &DistMessage) -> Result<Vec<u8>, DistError> {
        postcard::to_allocvec(msg).map_err(|e| DistError::Encode(e.to_string()))
    }

    fn decode(&self, bytes: &[u8]) -> Result<DistMessage, DistError> {
        postcard::from_bytes(bytes).map_err(|e| DistError::Decode(e.to_string()))
    }

    fn frame(&self, payload: Vec<u8>) -> Vec<u8> {
        let len = payload.len() as u32;
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    fn parse_frame(&self, buf: &[u8]) -> Result<Option<(DistMessage, usize)>, DistError> {
        if buf.len() < 4 {
            return Ok(None);
        }

        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

        if buf.len() < 4 + len {
            return Ok(None);
        }

        let msg = self.decode(&buf[4..4 + len])?;
        Ok(Some((msg, 4 + len)))
    }
}

// =============================================================================
// Transport Type Enum (for configuration)
// =============================================================================

/// Available transport types for distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportType {
    /// QUIC transport (default, Rust-to-Rust).
    #[default]
    Quic,
    /// TCP transport with Erlang distribution protocol (BEAM interop).
    Tcp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_postcard_protocol_roundtrip() {
        let protocol = PostcardProtocol;
        let msg = DistMessage::Ping { seq: 42 };

        let encoded = protocol.encode(&msg).unwrap();
        let decoded = protocol.decode(&encoded).unwrap();

        match decoded {
            DistMessage::Ping { seq } => assert_eq!(seq, 42),
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_postcard_protocol_framing() {
        let protocol = PostcardProtocol;
        let msg = DistMessage::Pong { seq: 123 };

        let encoded = protocol.encode(&msg).unwrap();
        let framed = protocol.frame(encoded);

        let (decoded, consumed) = protocol.parse_frame(&framed).unwrap().unwrap();
        assert_eq!(consumed, framed.len());

        match decoded {
            DistMessage::Pong { seq } => assert_eq!(seq, 123),
            _ => panic!("wrong message type"),
        }
    }
}
