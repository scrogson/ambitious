//! Room channel implementation using the Channel abstraction.
//!
//! This demonstrates how to use DREAM Channels for chat rooms.

use async_trait::async_trait;
use dream::channel::{Channel, HandleResult, JoinError, JoinResult, Socket};
use serde::{Deserialize, Serialize};

/// Custom state stored in each socket's assigns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomAssigns {
    /// The user's nickname.
    pub nick: String,
    /// The room name (extracted from topic).
    pub room_name: String,
}

/// Payload sent when joining a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinPayload {
    /// User's nickname.
    pub nick: String,
}

/// Events sent from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomInEvent {
    /// Send a message to the room.
    NewMsg { text: String },
    /// Update nickname.
    UpdateNick { nick: String },
}

/// Events broadcast to room members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomOutEvent {
    /// A user joined the room.
    UserJoined { nick: String },
    /// A user left the room.
    UserLeft { nick: String },
    /// A message was sent.
    Message { from: String, text: String },
    /// Presence update (who's in the room).
    PresenceState { users: Vec<String> },
}

/// The room channel handler.
pub struct RoomChannel;

#[async_trait]
impl Channel for RoomChannel {
    type Assigns = RoomAssigns;
    type JoinPayload = JoinPayload;
    type InEvent = RoomInEvent;
    type OutEvent = RoomOutEvent;

    fn topic_pattern() -> &'static str {
        "room:*"
    }

    async fn join(
        topic: &str,
        payload: Self::JoinPayload,
        socket: Socket<()>,
    ) -> JoinResult<Self::Assigns> {
        // Extract room name from topic
        let room_name = match topic.strip_prefix("room:") {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => {
                return JoinResult::Error(JoinError::new("invalid room topic"));
            }
        };

        // Validate nickname
        if payload.nick.is_empty() || payload.nick.len() > 32 {
            return JoinResult::Error(JoinError::new("nickname must be 1-32 characters"));
        }

        tracing::info!(room = %room_name, nick = %payload.nick, "User joining room");

        // Set up assigns with user info
        let assigns = RoomAssigns {
            nick: payload.nick,
            room_name,
        };

        JoinResult::Ok(socket.assign(assigns))
    }

    async fn handle_in(
        event: &str,
        payload: Self::InEvent,
        socket: &mut Socket<Self::Assigns>,
    ) -> HandleResult<Self::OutEvent> {
        match (event, payload) {
            ("new_msg", RoomInEvent::NewMsg { text }) => {
                tracing::debug!(
                    room = %socket.assigns.room_name,
                    from = %socket.assigns.nick,
                    text = %text,
                    "Broadcasting message"
                );

                // Broadcast to all room members
                HandleResult::Broadcast {
                    event: "new_msg".to_string(),
                    payload: RoomOutEvent::Message {
                        from: socket.assigns.nick.clone(),
                        text,
                    },
                }
            }
            ("update_nick", RoomInEvent::UpdateNick { nick }) => {
                if nick.is_empty() || nick.len() > 32 {
                    return HandleResult::Reply {
                        status: dream::channel::ReplyStatus::Error,
                        payload: b"nickname must be 1-32 characters".to_vec(),
                    };
                }

                socket.assigns.nick = nick;
                HandleResult::NoReply
            }
            _ => {
                tracing::warn!(event = %event, "Unknown event");
                HandleResult::NoReply
            }
        }
    }

    async fn terminate(reason: dream::channel::TerminateReason, socket: &Socket<Self::Assigns>) {
        tracing::info!(
            room = %socket.assigns.room_name,
            nick = %socket.assigns.nick,
            reason = ?reason,
            "User leaving room"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dream::channel::topic_matches;

    #[test]
    fn test_room_channel_pattern() {
        assert!(topic_matches(RoomChannel::topic_pattern(), "room:lobby"));
        assert!(topic_matches(RoomChannel::topic_pattern(), "room:123"));
        assert!(!topic_matches(RoomChannel::topic_pattern(), "user:123"));
    }
}
