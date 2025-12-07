//! Lobby channel implementation.
//!
//! The lobby channel provides system-level operations like listing rooms.
//! Clients join "lobby:main" to access these features.

use ambitious::Message;
use ambitious::channel::{Channel, HandleResult, JoinResult, ReplyStatus, Socket, async_trait};
use serde::{Deserialize, Serialize};

use crate::protocol::RoomInfo;
use crate::registry::Registry;

/// Payload sent when joining the lobby.
#[derive(Debug, Clone, Message)]
pub struct LobbyJoin {
    /// Optional nickname.
    pub nick: Option<String>,
}

/// All incoming events for the lobby channel.
#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum LobbyIn {
    /// Request to list available rooms.
    ListRooms,
}

/// All outgoing events for the lobby channel.
#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum LobbyOut {
    /// Response containing the list of rooms.
    RoomList {
        /// List of available rooms.
        rooms: Vec<RoomInfo>,
    },
}

/// The lobby channel - struct IS the state.
pub struct LobbyChannel {
    /// Optional nickname (not required for lobby).
    #[allow(dead_code)]
    nick: Option<String>,
}

#[async_trait]
impl Channel for LobbyChannel {
    type Join = LobbyJoin;
    type In = LobbyIn;
    type Out = LobbyOut;
    type Info = ();
    const TOPIC_PATTERN: &'static str = "lobby:*";

    async fn join(_topic: &str, payload: LobbyJoin, _socket: &Socket) -> JoinResult<Self> {
        tracing::info!(nick = ?payload.nick, "Client joining lobby");
        JoinResult::Ok(LobbyChannel { nick: payload.nick })
    }

    async fn handle_in(
        &mut self,
        _event: &str,
        msg: LobbyIn,
        _socket: &Socket,
    ) -> HandleResult<LobbyOut> {
        match msg {
            LobbyIn::ListRooms => {
                let rooms = Registry::list_rooms().await;
                tracing::debug!(room_count = rooms.len(), "Sending room list");

                HandleResult::Reply {
                    status: ReplyStatus::Ok,
                    payload: LobbyOut::RoomList { rooms },
                }
            }
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
