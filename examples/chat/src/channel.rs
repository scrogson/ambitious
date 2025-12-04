//! Room channel implementation using the Channel abstraction.
//!
//! This demonstrates how to use Starlang Channels for chat rooms,
//! including Phoenix-style Presence tracking for real-time user lists.

use crate::protocol::HistoryMessage;
use crate::room::{Room, RoomCall, RoomReply};
use crate::room_supervisor;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use starlang::RawTerm;
use starlang::channel::{
    Channel, ChannelReply, HandleResult, JoinError, JoinResult, Socket, broadcast_from, push,
};
use starlang::presence;
use std::time::Duration;

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
    /// Message history for newly joined users.
    History { messages: Vec<HistoryMessage> },
}

/// Metadata tracked in Presence for each user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPresenceMeta {
    /// User's nickname.
    pub nick: String,
    /// User's online status.
    pub status: String,
}

/// Internal messages for the channel (handle_info).
#[derive(Debug, Clone, Serialize, Deserialize)]
enum ChannelInfo {
    /// Trigger after-join logic (broadcast user joined).
    AfterJoin,
    /// Trigger presence state push (delayed to allow sync).
    PushPresenceState,
    /// A presence sync request from another user.
    PresenceSyncRequest { from_pid: String },
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

        // Get or create the room GenServer (this registers it globally)
        if let Err(e) = room_supervisor::get_or_create_room(&room_name).await {
            tracing::warn!(room = %room_name, error = ?e, "Failed to create room");
        }

        // Track presence for this user in the room
        let presence_key = format!("user:{}", socket.pid);
        let presence_meta = UserPresenceMeta {
            nick: payload.nick.clone(),
            status: "online".to_string(),
        };
        presence::track_pid(topic, &presence_key, socket.pid, presence_meta);
        tracing::debug!(topic = %topic, key = %presence_key, "Tracked presence");

        // Send ourselves an :after_join message to trigger presence sync and history push
        if let Ok(msg) = postcard::to_allocvec(&ChannelInfo::AfterJoin) {
            let _ = starlang::send_raw(socket.pid, msg);
        }

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
                    return HandleResult::ReplyRaw {
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

    async fn handle_info(
        msg: RawTerm,
        socket: &mut Socket<Self::Assigns>,
    ) -> HandleResult<Self::OutEvent> {
        // First try to decode as ChannelInfo (internal messages)
        if let Some(info) = msg.decode::<ChannelInfo>() {
            match info {
                ChannelInfo::AfterJoin => {
                    tracing::debug!(
                        room = %socket.assigns.room_name,
                        nick = %socket.assigns.nick,
                        "After join - broadcasting user joined and pushing history"
                    );

                    // Broadcast UserJoined to notify others (not ourselves)
                    broadcast_from(
                        socket,
                        "user_joined",
                        &RoomOutEvent::UserJoined {
                            nick: socket.assigns.nick.clone(),
                        },
                    );

                    // Push history directly (not via another self-message to avoid loop)
                    if let Some(room_pid) = room_supervisor::get_room(&socket.assigns.room_name) {
                        if let Ok(RoomReply::History(messages)) =
                            starlang::gen_server::call::<Room>(
                                room_pid,
                                RoomCall::GetHistory,
                                Duration::from_secs(5),
                            )
                            .await
                        {
                            if !messages.is_empty() {
                                tracing::debug!(
                                    room = %socket.assigns.room_name,
                                    message_count = messages.len(),
                                    "Pushing history to user"
                                );
                                push(socket, "history", &RoomOutEvent::History { messages });
                            }
                        }
                    }

                    // Schedule a delayed message to push presence state
                    // This gives time for presence sync responses to arrive from other nodes
                    let pid = socket.pid;
                    starlang::spawn(move || async move {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        if let Ok(msg) = postcard::to_allocvec(&ChannelInfo::PushPresenceState) {
                            let _ = starlang::send_raw(pid, msg);
                        }
                    });

                    return HandleResult::NoReply;
                }
                ChannelInfo::PushPresenceState => {
                    // Get current presence state and push to joining user
                    let topic = format!("room:{}", socket.assigns.room_name);
                    let presences = starlang::presence::list(&topic);
                    let mut users: Vec<String> = presences
                        .values()
                        .flat_map(|state| {
                            state.metas.iter().filter_map(|meta| {
                                meta.decode::<UserPresenceMeta>().map(|m| m.nick)
                            })
                        })
                        .collect();

                    // Always include self in the user list
                    if !users.contains(&socket.assigns.nick) {
                        users.push(socket.assigns.nick.clone());
                    }

                    tracing::debug!(
                        room = %socket.assigns.room_name,
                        user_count = users.len(),
                        users = ?users,
                        "Pushing presence state to user"
                    );
                    push(
                        socket,
                        "presence_state",
                        &RoomOutEvent::PresenceState { users },
                    );

                    return HandleResult::NoReply;
                }
                ChannelInfo::PresenceSyncRequest { from_pid: _ } => {
                    // Someone is asking who's here - respond with our nick
                    tracing::debug!(
                        room = %socket.assigns.room_name,
                        nick = %socket.assigns.nick,
                        "Responding to presence sync request"
                    );

                    broadcast_from(
                        socket,
                        "presence_sync_response",
                        &RoomOutEvent::PresenceSyncResponse {
                            nick: socket.assigns.nick.clone(),
                        },
                    );

                    return HandleResult::NoReply;
                }
            }
        }

        // Try to decode as ChannelReply (broadcast from another user via pg)
        if let Some(reply) = msg.decode::<ChannelReply>() {
            if let ChannelReply::Push { event, payload, .. } = reply {
                // Decode the room event from the payload bytes
                if let Ok(room_event) = postcard::from_bytes::<RoomOutEvent>(&payload) {
                    match room_event {
                        RoomOutEvent::PresenceSyncRequest { from_pid } => {
                            // Forward to our internal handler
                            if let Ok(info_msg) =
                                postcard::to_allocvec(&ChannelInfo::PresenceSyncRequest {
                                    from_pid,
                                })
                            {
                                let _ = starlang::send_raw(socket.pid, info_msg);
                            }
                        }
                        _ => {
                            // Other broadcasts are handled by the transport layer
                            tracing::trace!(event = %event, "Received broadcast in handle_info");
                        }
                    }
                }
            }
        }

        // Try to decode as PresenceMessage (delta from another node)
        if let Some(presence_msg) = msg.decode::<starlang::presence::PresenceMessage>() {
            // Apply the presence delta to the global tracker
            let from_node = starlang::core::node::node_name_atom(); // TODO: get actual source node
            starlang::presence::tracker().handle_message(presence_msg, from_node);
        }

        HandleResult::NoReply
    }

    async fn terminate(reason: starlang::channel::TerminateReason, socket: &Socket<Self::Assigns>) {
        tracing::info!(
            room = %socket.assigns.room_name,
            nick = %socket.assigns.nick,
            reason = ?reason,
            "User leaving room"
        );

        // Broadcast that we're leaving
        broadcast_from(
            socket,
            "user_left",
            &RoomOutEvent::UserLeft {
                nick: socket.assigns.nick.clone(),
            },
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
