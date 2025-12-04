//! Wire protocol for distribution.
//!
//! Defines the message types sent between nodes.

use crate::core::{Atom, Pid};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Distribution protocol messages.
///
/// These messages are serialized with postcard and sent over QUIC streams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DistMessage {
    // === Handshake ===
    /// Initial hello from connecting node.
    Hello {
        /// The connecting node's name.
        node_name: String,
        /// The connecting node's creation number.
        creation: u32,
    },

    /// Response to Hello, accepting the connection.
    Welcome {
        /// The receiving node's name.
        node_name: String,
        /// The receiving node's creation number.
        creation: u32,
    },

    // === Messaging ===
    /// Send a message to a process on this node.
    Send {
        /// Target process.
        to: Pid,
        /// Sender process (for replies).
        from: Option<Pid>,
        /// Serialized message payload.
        payload: Vec<u8>,
    },

    // === Node Monitoring ===
    /// Request to be notified when this node goes down.
    MonitorNode {
        /// The process that wants to be notified.
        requesting_pid: Pid,
    },

    /// Cancel a node monitor request.
    DemonitorNode {
        /// The process that no longer wants notifications.
        requesting_pid: Pid,
    },

    /// Notification that a node is going down (sent before disconnect).
    NodeGoingDown {
        /// Reason for shutdown.
        reason: String,
    },

    // === Process Monitoring ===
    /// Request to monitor a process on this node.
    MonitorProcess {
        /// The process requesting the monitor (will receive DOWN).
        from: Pid,
        /// The process to monitor on this node.
        target: Pid,
        /// Unique reference for this monitor.
        reference: u64,
    },

    /// Cancel a process monitor.
    DemonitorProcess {
        /// The process that was monitoring.
        from: Pid,
        /// The monitored process.
        target: Pid,
        /// The monitor reference.
        reference: u64,
    },

    /// Notification that a monitored process has exited.
    ProcessDown {
        /// The monitor reference.
        reference: u64,
        /// The process that exited.
        pid: Pid,
        /// The exit reason.
        reason: String,
    },

    // === Process Linking ===
    /// Request to link two processes.
    Link {
        /// Process on the sending node.
        from: Pid,
        /// Process on this node to link with.
        target: Pid,
    },

    /// Remove a link between processes.
    Unlink {
        /// Process on the sending node.
        from: Pid,
        /// Process on this node.
        target: Pid,
    },

    /// Exit signal from a linked process.
    Exit {
        /// The process that exited.
        from: Pid,
        /// The linked process that should receive this signal.
        target: Pid,
        /// The exit reason.
        reason: String,
    },

    // === Heartbeat ===
    /// Ping to check if connection is alive.
    Ping {
        /// Sequence number for matching pongs.
        seq: u64,
    },

    /// Response to ping.
    Pong {
        /// Sequence number from the ping.
        seq: u64,
    },

    // === Global Registry ===
    /// Global registry synchronization message.
    GlobalRegistry {
        /// Serialized GlobalRegistryMessage.
        payload: Vec<u8>,
    },

    // === Process Groups (pg) ===
    /// Process groups synchronization message.
    ProcessGroups {
        /// Serialized PgMessage.
        payload: Vec<u8>,
    },
}

impl DistMessage {
    /// Serialize this message to bytes.
    pub fn encode(&self) -> Result<Vec<u8>, DistError> {
        postcard::to_allocvec(self).map_err(|e| DistError::Encode(e.to_string()))
    }

    /// Deserialize a message from bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, DistError> {
        postcard::from_bytes(bytes).map_err(|e| DistError::Decode(e.to_string()))
    }
}

/// Errors that can occur in distribution.
#[derive(Debug, Clone)]
pub enum DistError {
    /// Message encoding failed.
    Encode(String),
    /// Message decoding failed.
    Decode(String),
    /// Connection failed.
    Connect(String),
    /// Not connected to the specified node.
    NotConnected(Atom),
    /// Distribution not initialized.
    NotInitialized,
    /// Address parsing failed.
    InvalidAddress(String),
    /// TLS/certificate error.
    Tls(String),
    /// I/O error.
    Io(String),
    /// Handshake failed.
    Handshake(String),
    /// Node already connected.
    AlreadyConnected(Atom),
    /// Connection closed.
    ConnectionClosed,
}

impl fmt::Display for DistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DistError::Encode(e) => write!(f, "encode error: {}", e),
            DistError::Decode(e) => write!(f, "decode error: {}", e),
            DistError::Connect(e) => write!(f, "connection error: {}", e),
            DistError::NotConnected(node) => write!(f, "not connected to node {}", node),
            DistError::NotInitialized => write!(f, "distribution not initialized"),
            DistError::InvalidAddress(e) => write!(f, "invalid address: {}", e),
            DistError::Tls(e) => write!(f, "TLS error: {}", e),
            DistError::Io(e) => write!(f, "I/O error: {}", e),
            DistError::Handshake(e) => write!(f, "handshake failed: {}", e),
            DistError::AlreadyConnected(node) => write!(f, "already connected to node {}", node),
            DistError::ConnectionClosed => write!(f, "connection closed"),
        }
    }
}

impl std::error::Error for DistError {}

/// Frame a message with a length prefix.
///
/// Format: 4-byte big-endian length + payload
pub fn frame_message(msg: &DistMessage) -> Result<Vec<u8>, DistError> {
    let payload = msg.encode()?;
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Try to parse a framed message from a buffer.
///
/// Returns `Some((message, bytes_consumed))` if a complete message is available,
/// or `None` if more data is needed.
pub fn parse_frame(buf: &[u8]) -> Result<Option<(DistMessage, usize)>, DistError> {
    if buf.len() < 4 {
        return Ok(None);
    }

    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

    if buf.len() < 4 + len {
        return Ok(None);
    }

    let msg = DistMessage::decode(&buf[4..4 + len])?;
    Ok(Some((msg, 4 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_roundtrip() {
        let msg = DistMessage::Hello {
            node_name: "test@localhost".to_string(),
            creation: 42,
        };

        let encoded = msg.encode().unwrap();
        let decoded = DistMessage::decode(&encoded).unwrap();

        match decoded {
            DistMessage::Hello {
                node_name,
                creation,
            } => {
                assert_eq!(node_name, "test@localhost");
                assert_eq!(creation, 42);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_frame_roundtrip() {
        let msg = DistMessage::Ping { seq: 123 };
        let frame = frame_message(&msg).unwrap();

        let (decoded, consumed) = parse_frame(&frame).unwrap().unwrap();
        assert_eq!(consumed, frame.len());

        match decoded {
            DistMessage::Ping { seq } => assert_eq!(seq, 123),
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_parse_frame_incomplete() {
        // Less than 4 bytes - no length header yet
        assert!(parse_frame(&[0, 1, 2]).unwrap().is_none());

        // Has length but not enough payload
        let msg = DistMessage::Ping { seq: 1 };
        let frame = frame_message(&msg).unwrap();
        assert!(parse_frame(&frame[..frame.len() - 1]).unwrap().is_none());
    }
}
