//! Room channel implementation using the Channel abstraction.
//!
//! This demonstrates how to use Starlang Channels for chat rooms,
//! including Phoenix-style Presence tracking for real-time user lists.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use starlang::channel::{Channel, HandleResult, JoinError, JoinResult, Socket};
use starlang::presence;

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
    /// Request for presence sync (new joiner wants to know who's here).
    PresenceSyncRequest { from_pid: String },
    /// Response to presence sync (existing member announces themselves).
    PresenceSyncResponse { nick: String },
}

/// Metadata tracked in Presence for each user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPresenceMeta {
    /// User's nickname.
    pub nick: String,
    /// User's online status.
    pub status: String,
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

        // Track presence for this user in the room
        let presence_key = format!("user:{}", socket.pid);
        let presence_meta = UserPresenceMeta {
            nick: payload.nick.clone(),
            status: "online".to_string(),
        };
        presence::track_pid(topic, &presence_key, socket.pid, presence_meta);
        tracing::debug!(topic = %topic, key = %presence_key, "Tracked presence");

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
                        status: starlang::channel::ReplyStatus::Error,
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

    async fn terminate(reason: starlang::channel::TerminateReason, socket: &Socket<Self::Assigns>) {
        tracing::info!(
            room = %socket.assigns.room_name,
            nick = %socket.assigns.nick,
            reason = ?reason,
            "User leaving room"
        );

        // Untrack presence for this user
        let topic = format!("room:{}", socket.assigns.room_name);
        let presence_key = format!("user:{}", socket.pid);
        presence::untrack_pid(&topic, &presence_key, socket.pid);
        tracing::debug!(topic = %topic, key = %presence_key, "Untracked presence");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starlang::channel::topic_matches;

    #[test]
    fn test_room_channel_pattern() {
        assert!(topic_matches(RoomChannel::topic_pattern(), "room:lobby"));
        assert!(topic_matches(RoomChannel::topic_pattern(), "room:123"));
        assert!(!topic_matches(RoomChannel::topic_pattern(), "user:123"));
    }
}
