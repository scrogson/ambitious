//! Room channel implementation using the Channel abstraction.
//!
//! This demonstrates how to use Ambitious Channels for chat rooms,
//! including Phoenix-style Presence tracking for real-time user lists.
//!
//! In the new API, the struct IS the channel state. `join` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::HistoryMessage;
use crate::room::{Room, RoomCall, RoomReply};
use crate::room_supervisor;
use ambitious::Message;
use ambitious::RawTerm;
use ambitious::channel::{
    Channel, ChannelReply, HandleIn, HandleResult, JoinError, JoinResult, RawHandleResult,
    ReplyStatus, Socket, TerminateReason, broadcast_from, push,
};
use ambitious::gen_server::v3::call;
use ambitious::presence::{Presence, PresenceMessage};
use ambitious::pubsub::PubSub;
use ambitious::{channel, handle_in};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The name of the chat presence server.
const PRESENCE_NAME: &str = "chat_presence";
/// The name of the chat pubsub server.
const PUBSUB_NAME: &str = "chat_pubsub";

/// Payload sent when joining a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinPayload {
    /// User's nickname.
    pub nick: String,
}

/// Message to send a new chat message.
#[derive(Debug, Clone, Message)]
pub struct NewMsg {
    /// Message text.
    pub text: String,
}

/// Broadcast payload for new messages.
#[derive(Debug, Clone, Message)]
pub struct MessageBroadcast {
    /// Who sent the message.
    pub from: String,
    /// Message text.
    pub text: String,
}

/// Message to update nickname.
#[derive(Debug, Clone, Message)]
pub struct UpdateNick {
    /// New nickname.
    pub nick: String,
}

/// Reply for nick update errors.
#[derive(Debug, Clone, Message)]
pub struct NickError {
    /// Error message.
    pub error: String,
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
        Self {
            magic: CHANNEL_INFO_MAGIC,
            info,
        }
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

/// The room channel - struct IS the state.
pub struct RoomChannel {
    /// The user's nickname.
    nick: String,
    /// The room name (extracted from topic).
    room_name: String,
}

#[channel(topic = "room:*")]
impl Channel for RoomChannel {
    type JoinPayload = JoinPayload;

    async fn join(topic: &str, payload: Self::JoinPayload, socket: &Socket) -> JoinResult<Self> {
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

        JoinResult::Ok(RoomChannel {
            nick: payload.nick,
            room_name,
        })
    }

    async fn handle_info_raw(&mut self, msg: RawTerm, socket: &Socket) -> RawHandleResult {
        // First try to decode as ChannelInfoMessage (internal messages with magic marker)
        if let Some(wrapped) = msg.decode::<ChannelInfoMessage>()
            && wrapped.is_valid()
        {
            match wrapped.info {
                ChannelInfo::AfterJoin => {
                    tracing::info!(
                        room = %self.room_name,
                        nick = %self.nick,
                        socket_pid = ?socket.pid,
                        "AfterJoin triggered - this socket will receive history"
                    );

                    // Broadcast UserJoined to notify others (not ourselves)
                    broadcast_from(
                        socket,
                        "user_joined",
                        &RoomOutEvent::UserJoined {
                            nick: self.nick.clone(),
                        },
                    );

                    // Push history directly (not via another self-message to avoid loop)
                    if let Some(room_pid) = room_supervisor::get_room(&self.room_name)
                        && let Ok(RoomReply::History(messages)) = call::<Room, RoomReply>(
                            room_pid,
                            RoomCall::GetHistory,
                            Duration::from_secs(5),
                        )
                        .await
                        && !messages.is_empty()
                    {
                        tracing::info!(
                            room = %self.room_name,
                            nick = %self.nick,
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
                        let _ = ambitious::send(
                            pid,
                            &ChannelInfoMessage::new(ChannelInfo::PushPresenceState),
                        );
                    });

                    return RawHandleResult::NoReply;
                }
                ChannelInfo::PushPresenceState => {
                    // Get current presence state and push to joining user
                    let topic = format!("room:{}", self.room_name);
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
                    if !users.contains(&self.nick) {
                        users.push(self.nick.clone());
                    }

                    tracing::debug!(
                        room = %self.room_name,
                        user_count = users.len(),
                        users = ?users,
                        "Pushing presence state to user"
                    );
                    push(
                        socket,
                        "presence_state",
                        &RoomOutEvent::PresenceState { users },
                    );

                    return RawHandleResult::NoReply;
                }
                ChannelInfo::PresenceSyncRequest { from_pid: _ } => {
                    // Someone is asking who's here - respond with our nick
                    tracing::debug!(
                        room = %self.room_name,
                        nick = %self.nick,
                        "Responding to presence sync request"
                    );

                    broadcast_from(
                        socket,
                        "presence_sync_response",
                        &RoomOutEvent::PresenceSyncResponse {
                            nick: self.nick.clone(),
                        },
                    );

                    return RawHandleResult::NoReply;
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
                        let _ = ambitious::send(
                            socket.pid,
                            &ChannelInfoMessage::new(ChannelInfo::PresenceSyncRequest { from_pid }),
                        );
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
                    for state in diff.joins.values() {
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
                    for state in diff.leaves.values() {
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
                    return RawHandleResult::NoReply;
                }
                PresenceMessage::StateSync { .. } => {
                    // Full state sync - we handle this via PushPresenceState
                }
                PresenceMessage::SyncRequest { .. } | PresenceMessage::SyncResponse { .. } => {
                    // These are handled by the Presence server, not channels
                }
            }
        }

        RawHandleResult::NoReply
    }

    async fn terminate(&mut self, reason: TerminateReason, socket: &Socket) {
        tracing::info!(
            room = %self.room_name,
            nick = %self.nick,
            reason = ?reason,
            "User leaving room"
        );

        // Broadcast that we're leaving
        broadcast_from(
            socket,
            "user_left",
            &RoomOutEvent::UserLeft {
                nick: self.nick.clone(),
            },
        );

        // Untrack presence for this user
        let topic = format!("room:{}", self.room_name);
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

// =============================================================================
// Typed Event Handlers
// =============================================================================

#[handle_in("new_msg")]
impl HandleIn<NewMsg> for RoomChannel {
    type Reply = MessageBroadcast;

    async fn handle_in(&mut self, msg: NewMsg, _socket: &Socket) -> HandleResult<MessageBroadcast> {
        tracing::debug!(
            room = %self.room_name,
            from = %self.nick,
            text = %msg.text,
            "Broadcasting message"
        );

        HandleResult::Broadcast {
            event: "new_msg".to_string(),
            payload: MessageBroadcast {
                from: self.nick.clone(),
                text: msg.text,
            },
        }
    }
}

#[handle_in("update_nick")]
impl HandleIn<UpdateNick> for RoomChannel {
    type Reply = NickError;

    async fn handle_in(&mut self, msg: UpdateNick, _socket: &Socket) -> HandleResult<NickError> {
        if msg.nick.is_empty() || msg.nick.len() > 32 {
            return HandleResult::Reply {
                status: ReplyStatus::Error,
                payload: NickError {
                    error: "nickname must be 1-32 characters".to_string(),
                },
            };
        }

        self.nick = msg.nick;
        HandleResult::NoReply
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
