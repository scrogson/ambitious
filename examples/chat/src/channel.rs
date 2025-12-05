//! Room channel implementation using the Channel abstraction.
//!
//! This demonstrates how to use Ambitious Channels for chat rooms,
//! including Phoenix-style Presence tracking for real-time user lists.

use crate::protocol::HistoryMessage;
use crate::room::{Room, RoomCall, RoomReply};
use crate::room_supervisor;
use ambitious::channel::{
    Channel, ChannelReply, HandleResult, JoinError, JoinResult, Socket, broadcast_from, push,
};
use ambitious::presence::{Presence, PresenceMessage};
use ambitious::pubsub::PubSub;
use ambitious::RawTerm;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The name of the chat presence server.
const PRESENCE_NAME: &str = "chat_presence";
/// The name of the chat pubsub server.
const PUBSUB_NAME: &str = "chat_pubsub";
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
///
/// These are sent to the channel's own process (via send_raw to socket.pid)
/// and handled in handle_info. They are NOT broadcast messages.
///
/// IMPORTANT: Uses a magic marker to prevent accidental deserialization of other
/// message types (like PresenceMessage::Delta) as ChannelInfo. Without this,
/// postcard's compact encoding can cause false positive matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelInfoMessage {
    /// Magic marker - must be CHANNEL_INFO_MAGIC for valid messages
    magic: u64,
    /// The actual message
    info: ChannelInfo,
}

/// Magic number to identify valid ChannelInfo messages
const CHANNEL_INFO_MAGIC: u64 = 0xC4A7_14F0_DEAD_BEEF;

impl ChannelInfoMessage {
    fn new(info: ChannelInfo) -> Self {
        Self { magic: CHANNEL_INFO_MAGIC, info }
    }

    fn is_valid(&self) -> bool {
        self.magic == CHANNEL_INFO_MAGIC
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ChannelInfo {
    /// Trigger after-join logic (broadcast user joined, push history).
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
        if let Err(e) = Presence::track_pid(
            PRESENCE_NAME,
            topic,
            &presence_key,
            socket.pid,
            &presence_meta,
        )
        .await
        {
            tracing::warn!(error = %e, "Failed to track presence");
        }
        tracing::debug!(topic = %topic, key = %presence_key, "Tracked presence");

        // Subscribe to presence updates for this room
        let presence_topic = format!("presence:{}", topic);
        if let Err(e) = PubSub::subscribe_pid(PUBSUB_NAME, &presence_topic, socket.pid).await {
            tracing::warn!(error = %e, "Failed to subscribe to presence updates");
        }
        tracing::debug!(topic = %presence_topic, "Subscribed to presence updates");

        // Send ourselves an :after_join message to trigger presence sync and history push
        let _ = ambitious::send(socket.pid, &ChannelInfoMessage::new(ChannelInfo::AfterJoin));

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
                        status: ambitious::channel::ReplyStatus::Error,
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
        // First try to decode as ChannelInfoMessage (internal messages with magic marker)
        if let Some(wrapped) = msg.decode::<ChannelInfoMessage>()
            && wrapped.is_valid()
        {
            match wrapped.info {
                ChannelInfo::AfterJoin => {
                    tracing::info!(
                        room = %socket.assigns.room_name,
                        nick = %socket.assigns.nick,
                        socket_pid = ?socket.pid,
                        "AfterJoin triggered - this socket will receive history"
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
                    if let Some(room_pid) = room_supervisor::get_room(&socket.assigns.room_name)
                        && let Ok(RoomReply::History(messages)) =
                            ambitious::gen_server::call::<Room>(
                                room_pid,
                                RoomCall::GetHistory,
                                Duration::from_secs(5),
                            )
                            .await
                        && !messages.is_empty()
                    {
                        tracing::info!(
                            room = %socket.assigns.room_name,
                            nick = %socket.assigns.nick,
                            socket_pid = ?socket.pid,
                            message_count = messages.len(),
                            "PUSHING HISTORY to socket"
                        );
                        push(socket, "history", &RoomOutEvent::History { messages });
                    }

                    // Schedule a delayed message to push presence state
                    // This gives time for presence sync responses to arrive from other nodes
                    let pid = socket.pid;
                    ambitious::spawn(move || async move {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let _ = ambitious::send(pid, &ChannelInfoMessage::new(ChannelInfo::PushPresenceState));
                    });

                    return HandleResult::NoReply;
                }
                ChannelInfo::PushPresenceState => {
                    // Get current presence state and push to joining user
                    let topic = format!("room:{}", socket.assigns.room_name);
                    let presences = match Presence::list(PRESENCE_NAME, &topic).await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to get presence list");
                            std::collections::HashMap::new()
                        }
                    };
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
        if let Some(reply) = msg.decode::<ChannelReply>()
            && let ChannelReply::Push { event, payload, .. } = reply
        {
            // Decode the room event from the payload bytes
            let payload_raw = RawTerm::from(payload);
            if let Some(room_event) = payload_raw.decode::<RoomOutEvent>() {
                match room_event {
                    RoomOutEvent::PresenceSyncRequest { from_pid } => {
                        // Forward to our internal handler
                        let _ = ambitious::send(socket.pid, &ChannelInfoMessage::new(ChannelInfo::PresenceSyncRequest { from_pid }));
                    }
                    _ => {
                        // Other broadcasts are handled by the transport layer
                        tracing::trace!(event = %event, "Received broadcast in handle_info");
                    }
                }
            }
        }

        // Handle presence delta messages from PubSub
        if let Some(presence_msg) = msg.decode::<PresenceMessage>() {
            match presence_msg {
                PresenceMessage::Delta { topic: _, diff } => {
                    // Process joins - notify client of new users
                    for (_key, state) in &diff.joins {
                        for meta in &state.metas {
                            if let Some(user_meta) = meta.decode::<UserPresenceMeta>() {
                                // Don't notify about ourselves
                                if meta.pid != socket.pid {
                                    push(
                                        socket,
                                        "user_joined",
                                        &RoomOutEvent::UserJoined {
                                            nick: user_meta.nick,
                                        },
                                    );
                                }
                            }
                        }
                    }

                    // Process leaves - notify client of users leaving
                    for (_key, state) in &diff.leaves {
                        for meta in &state.metas {
                            if let Some(user_meta) = meta.decode::<UserPresenceMeta>() {
                                // Don't notify about ourselves
                                if meta.pid != socket.pid {
                                    push(
                                        socket,
                                        "user_left",
                                        &RoomOutEvent::UserLeft {
                                            nick: user_meta.nick,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    return HandleResult::NoReply;
                }
                PresenceMessage::StateSync { .. } => {
                    // Full state sync - we handle this via PushPresenceState
                }
                PresenceMessage::SyncRequest { .. } | PresenceMessage::SyncResponse { .. } => {
                    // These are handled by the Presence server, not channels
                }
            }
        }

        HandleResult::NoReply
    }

    async fn terminate(
        reason: ambitious::channel::TerminateReason,
        socket: &Socket<Self::Assigns>,
    ) {
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
        if let Err(e) =
            Presence::untrack_pid(PRESENCE_NAME, &topic, &presence_key, socket.pid).await
        {
            tracing::warn!(error = %e, "Failed to untrack presence");
        }
        tracing::debug!(topic = %topic, key = %presence_key, "Untracked presence");

        // Unsubscribe from presence updates
        let presence_topic = format!("presence:{}", topic);
        if let Err(e) = PubSub::unsubscribe_pid(PUBSUB_NAME, &presence_topic, socket.pid).await {
            tracing::warn!(error = %e, "Failed to unsubscribe from presence updates");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ambitious::channel::topic_matches;

    #[test]
    fn test_room_channel_pattern() {
        assert!(topic_matches(RoomChannel::topic_pattern(), "room:lobby"));
        assert!(topic_matches(RoomChannel::topic_pattern(), "room:123"));
        assert!(!topic_matches(RoomChannel::topic_pattern(), "user:123"));
    }
}
