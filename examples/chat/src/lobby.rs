//! Lobby channel implementation.
//!
//! The lobby channel provides system-level operations like listing rooms.
//! Clients join "lobby:main" to access these features.

use ambitious::channel::{
    Channel, HandleIn, HandleResult, JoinResult, ReplyStatus, Socket, async_trait,
};
use ambitious::{Message, handle_in};
use serde::{Deserialize, Serialize};

use crate::protocol::RoomInfo;
use crate::registry::Registry;

/// Payload sent when joining the lobby.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyJoinPayload {
    /// Optional nickname.
    #[serde(default)]
    pub nick: Option<String>,
}

/// Request to list available rooms.
#[derive(Debug, Clone, Message)]
pub struct ListRooms;

/// Response containing the list of rooms.
#[derive(Debug, Clone, Message)]
pub struct RoomList {
    /// List of available rooms.
    pub rooms: Vec<RoomInfo>,
}

/// The lobby channel - struct IS the state.
pub struct LobbyChannel {
    /// Optional nickname (not required for lobby).
    #[allow(dead_code)]
    nick: Option<String>,
}

#[async_trait]
impl Channel for LobbyChannel {
    type JoinPayload = LobbyJoinPayload;
    const TOPIC_PATTERN: &'static str = "lobby:*";

    async fn join(_topic: &str, payload: Self::JoinPayload, _socket: &Socket) -> JoinResult<Self> {
        tracing::info!(nick = ?payload.nick, "Client joining lobby");
        JoinResult::Ok(LobbyChannel { nick: payload.nick })
    }
}

#[handle_in("list_rooms")]
impl HandleIn<ListRooms> for LobbyChannel {
    type Reply = RoomList;

    async fn handle_in(&mut self, _msg: ListRooms, _socket: &Socket) -> HandleResult<RoomList> {
        let rooms = Registry::list_rooms().await;
        tracing::debug!(room_count = rooms.len(), "Sending room list");

        HandleResult::Reply {
            status: ReplyStatus::Ok,
            payload: RoomList { rooms },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ambitious::channel::topic_matches;

    #[test]
    fn test_lobby_channel_pattern() {
        assert!(topic_matches(LobbyChannel::TOPIC_PATTERN, "lobby:main"));
        assert!(topic_matches(LobbyChannel::TOPIC_PATTERN, "lobby:system"));
        assert!(!topic_matches(LobbyChannel::TOPIC_PATTERN, "room:lobby"));
    }
}
