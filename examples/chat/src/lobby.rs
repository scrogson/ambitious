//! Lobby channel implementation.
//!
//! The lobby channel provides system-level operations like listing rooms.
//! Clients join "lobby:main" to access these features.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use starlang::channel::{Channel, HandleResult, JoinResult, Socket};

use crate::protocol::RoomInfo;
use crate::registry::Registry;

/// Custom state stored in the lobby socket's assigns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyAssigns {
    /// Optional nickname (not required for lobby).
    pub nick: Option<String>,
}

/// Payload sent when joining the lobby.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyJoinPayload {
    /// Optional nickname.
    #[serde(default)]
    pub nick: Option<String>,
}

/// Events sent from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LobbyInEvent {
    /// Request the list of rooms.
    ListRooms,
}

/// Events sent from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LobbyOutEvent {
    /// List of available rooms.
    RoomList { rooms: Vec<RoomInfo> },
}

/// The lobby channel handler.
pub struct LobbyChannel;

#[async_trait]
impl Channel for LobbyChannel {
    type Assigns = LobbyAssigns;
    type JoinPayload = LobbyJoinPayload;
    type InEvent = LobbyInEvent;
    type OutEvent = LobbyOutEvent;

    fn topic_pattern() -> &'static str {
        "lobby:*"
    }

    async fn join(
        _topic: &str,
        payload: Self::JoinPayload,
        socket: Socket<()>,
    ) -> JoinResult<Self::Assigns> {
        tracing::info!(nick = ?payload.nick, "Client joining lobby");

        let assigns = LobbyAssigns { nick: payload.nick };
        JoinResult::Ok(socket.assign(assigns))
    }

    async fn handle_in(
        event: &str,
        payload: Self::InEvent,
        _socket: &mut Socket<Self::Assigns>,
    ) -> HandleResult<Self::OutEvent> {
        match (event, payload) {
            ("list_rooms", LobbyInEvent::ListRooms) => {
                let rooms = Registry::list_rooms().await;
                tracing::debug!(room_count = rooms.len(), "Sending room list");

                // Return a typed reply with the room list
                HandleResult::Reply {
                    status: starlang::channel::ReplyStatus::Ok,
                    payload: LobbyOutEvent::RoomList { rooms },
                }
            }
            _ => {
                tracing::warn!(event = %event, "Unknown lobby event");
                HandleResult::NoReply
            }
        }
    }

    async fn handle_info(
        _msg: Vec<u8>,
        _socket: &mut Socket<Self::Assigns>,
    ) -> HandleResult<Self::OutEvent> {
        HandleResult::NoReply
    }

    async fn terminate(
        _reason: starlang::channel::TerminateReason,
        _socket: &Socket<Self::Assigns>,
    ) {
        tracing::debug!("Client leaving lobby");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starlang::channel::topic_matches;

    #[test]
    fn test_lobby_channel_pattern() {
        assert!(topic_matches(LobbyChannel::topic_pattern(), "lobby:main"));
        assert!(topic_matches(LobbyChannel::topic_pattern(), "lobby:system"));
        assert!(!topic_matches(LobbyChannel::topic_pattern(), "room:lobby"));
    }
}
