//! Chat protocol definitions.
//!
//! This module defines the wire protocol for the chat application.
//! Messages are serialized using postcard (a compact binary format).

use serde::{Deserialize, Serialize};

/// Commands sent from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientCommand {
    /// Set the user's nickname.
    Nick(String),
    /// Join a room (creates it if it doesn't exist).
    Join(String),
    /// Leave a room.
    Leave(String),
    /// Send a message to a room.
    Msg { room: String, text: String },
    /// List all rooms.
    ListRooms,
    /// List users in a room.
    ListUsers(String),
    /// Quit the chat.
    Quit,
}

/// Events sent from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerEvent {
    /// Welcome message with server info.
    Welcome { message: String },
    /// Acknowledgment of nickname change.
    NickOk { nick: String },
    /// Error setting nickname.
    NickError { reason: String },
    /// Successfully joined a room.
    Joined { room: String },
    /// Failed to join a room.
    JoinError { room: String, reason: String },
    /// Successfully left a room.
    Left { room: String },
    /// A chat message in a room.
    Message {
        room: String,
        from: String,
        text: String,
    },
    /// A user joined a room.
    UserJoined { room: String, nick: String },
    /// A user left a room.
    UserLeft { room: String, nick: String },
    /// List of rooms.
    RoomList { rooms: Vec<RoomInfo> },
    /// List of users in a room.
    UserList { room: String, users: Vec<String> },
    /// Generic error message.
    Error { message: String },
    /// Server is shutting down.
    Shutdown,
}

/// Information about a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub name: String,
    pub user_count: usize,
}

// Message trait is automatically implemented via blanket impl for all
// types that implement Serialize + DeserializeOwned + Send + 'static

/// Frame a message for sending over TCP.
/// Format: 4-byte big-endian length prefix + payload
pub fn frame_message<T: Serialize>(msg: &T) -> Vec<u8> {
    let payload = postcard::to_allocvec(msg).expect("serialization failed");
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    frame
}

/// Parse a framed message from bytes.
/// Returns the message and remaining bytes, or None if incomplete.
pub fn parse_frame<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Option<(T, usize)> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return None;
    }
    let msg: T = postcard::from_bytes(&buf[4..4 + len]).ok()?;
    Some((msg, 4 + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_roundtrip() {
        let cmd = ClientCommand::Msg {
            room: "general".to_string(),
            text: "Hello, world!".to_string(),
        };

        let frame = frame_message(&cmd);
        let (parsed, consumed): (ClientCommand, usize) = parse_frame(&frame).unwrap();

        assert_eq!(consumed, frame.len());
        match parsed {
            ClientCommand::Msg { room, text } => {
                assert_eq!(room, "general");
                assert_eq!(text, "Hello, world!");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_incomplete_frame() {
        let cmd = ClientCommand::Nick("alice".to_string());
        let frame = frame_message(&cmd);

        // Incomplete length prefix
        assert!(parse_frame::<ClientCommand>(&frame[..2]).is_none());

        // Incomplete payload
        assert!(parse_frame::<ClientCommand>(&frame[..frame.len() - 1]).is_none());
    }
}
