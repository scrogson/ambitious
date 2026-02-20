//! User session process using Channels and Transport trait.
//!
//! Each connected client gets a session process that:
//! - Reads commands from the TCP socket via Transport
//! - Sends events back to the client
//! - Uses ChannelServer for room management

use crate::channel::{RoomChannel, RoomJoin, RoomOutEvent};
use crate::protocol::{ClientCommand, ServerEvent, frame_message, parse_frame};
use crate::registry::Registry;
use crate::room::{Room, RoomCast};
use crate::room_supervisor;
use ambitious::channel::transport::{Transport, TransportConfig};
use ambitious::channel::{ChannelReply, ChannelServer, ChannelServerBuilder};
use ambitious::gen_server::cast;
use ambitious::{Pid, Term};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ============================================================================
// TCP Transport Implementation
// ============================================================================

/// TCP transport for the chat protocol.
///
/// Uses length-prefixed postcard framing: 4-byte big-endian length + payload.
pub struct TcpTransport {
    stream: TcpStream,
    /// Buffer for incomplete frames
    pending: Vec<u8>,
}

/// Error type for TCP transport operations.
#[derive(Debug)]
pub enum TcpTransportError {
    Io(std::io::Error),
}

impl std::fmt::Display for TcpTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TcpTransportError::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for TcpTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TcpTransportError::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for TcpTransportError {
    fn from(e: std::io::Error) -> Self {
        TcpTransportError::Io(e)
    }
}

#[async_trait]
impl Transport for TcpTransport {
    type Connection = TcpStream;
    type Error = TcpTransportError;

    async fn connect(
        conn: Self::Connection,
        _config: &TransportConfig,
    ) -> Result<Self, Self::Error> {
        Ok(TcpTransport {
            stream: conn,
            pending: Vec::new(),
        })
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
        // First check if we have a complete frame in pending buffer
        if let Some((cmd, consumed)) = parse_frame::<ClientCommand>(&self.pending) {
            self.pending.drain(..consumed);
            // Re-serialize for return (we need raw bytes, not the parsed command)
            // Actually, let's return the raw frame bytes
            let frame = frame_message(&cmd);
            return Ok(Some(frame));
        }

        // Read more data from socket
        let mut buf = [0u8; 4096];
        match self.stream.read(&mut buf).await {
            Ok(0) => Ok(None), // Connection closed
            Ok(n) => {
                self.pending.extend_from_slice(&buf[..n]);
                // Try to parse again
                if let Some((cmd, consumed)) = parse_frame::<ClientCommand>(&self.pending) {
                    self.pending.drain(..consumed);
                    let frame = frame_message(&cmd);
                    Ok(Some(frame))
                } else {
                    // Return empty to indicate no complete message yet
                    // Caller should call recv again
                    Ok(Some(Vec::new()))
                }
            }
            Err(e) => Err(TcpTransportError::Io(e)),
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.stream.write_all(data).await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.stream.shutdown().await?;
        Ok(())
    }
}

impl TcpTransport {
    /// Try to receive a complete command without blocking indefinitely.
    /// Returns None if no complete command is available.
    pub async fn try_recv_command(&mut self) -> Result<Option<ClientCommand>, TcpTransportError> {
        // Check pending buffer first
        if let Some((cmd, consumed)) = parse_frame::<ClientCommand>(&self.pending) {
            self.pending.drain(..consumed);
            return Ok(Some(cmd));
        }
        Ok(None)
    }

    /// Read more data into the pending buffer.
    pub async fn fill_buffer(&mut self) -> Result<bool, TcpTransportError> {
        let mut buf = [0u8; 4096];
        match self.stream.read(&mut buf).await {
            Ok(0) => Ok(false), // Connection closed
            Ok(n) => {
                self.pending.extend_from_slice(&buf[..n]);
                Ok(true)
            }
            Err(e) => Err(TcpTransportError::Io(e)),
        }
    }

    /// Send a server event to the client.
    pub async fn send_event(&mut self, event: &ServerEvent) -> Result<(), TcpTransportError> {
        let frame = frame_message(event);
        self.send(&frame).await
    }
}

// ============================================================================
// Session using Transport
// ============================================================================

/// User session state.
pub struct Session {
    /// TCP transport for this client.
    transport: TcpTransport,
    /// User's nickname.
    nick: Option<String>,
    /// Channel server for managing room connections.
    channels: ChannelServer,
}

impl Session {
    pub async fn new(stream: TcpStream, pid: Pid) -> Result<Self, TcpTransportError> {
        // Build channel server with RoomChannel handler
        let channels = ChannelServerBuilder::new()
            .channel::<RoomChannel>()
            .build(pid);

        // Create transport using the Transport trait
        let config = TransportConfig::new();
        let transport = TcpTransport::connect(stream, &config).await?;

        Ok(Self {
            transport,
            nick: None,
            channels,
        })
    }

    /// Run the session, processing commands until disconnect.
    pub async fn run(mut self) {
        // Send welcome message
        if self
            .transport
            .send_event(&ServerEvent::Welcome {
                message: "Welcome to Ambitious Chat! Use /nick <name> to set your nickname."
                    .to_string(),
            })
            .await
            .is_err()
        {
            return;
        }

        loop {
            // First, drain any pending process messages (non-blocking)
            while let Some(data) = ambitious::try_recv() {
                self.handle_channel_message(&data).await;
            }

            // Try to parse any pending commands
            match self.transport.try_recv_command().await {
                Ok(Some(cmd)) => {
                    if !self.handle_command(cmd).await {
                        return; // Client quit
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(error = %e, "Error parsing command");
                    self.cleanup().await;
                    return;
                }
            }

            // Wait for either TCP data or process messages
            tokio::select! {
                biased; // Prefer process messages over TCP

                // Check for messages from channels (broadcasts, pushes)
                msg = ambitious::recv_timeout(Duration::from_millis(100)) => {
                    if let Ok(Some(data)) = msg {
                        self.handle_channel_message(&data).await;
                    }
                }

                // Read more data from transport
                result = self.transport.fill_buffer() => {
                    match result {
                        Ok(true) => {
                            // Data received, try to parse commands
                            while let Ok(Some(cmd)) = self.transport.try_recv_command().await {
                                if !self.handle_command(cmd).await {
                                    return; // Client quit
                                }
                            }
                        }
                        Ok(false) => {
                            // Connection closed
                            tracing::info!(nick = ?self.nick, "Client disconnected");
                            self.cleanup().await;
                            return;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Read error");
                            self.cleanup().await;
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Handle incoming process messages.
    ///
    /// Messages fall into two categories:
    /// 1. ChannelReply::Push - broadcasts from pg groups, forwarded to the client
    /// 2. Everything else - internal messages dispatched to channel handle_info
    async fn handle_channel_message(&mut self, data: &[u8]) {
        use ambitious::RawTerm;

        let raw = RawTerm::from(data.to_vec());

        // First, check if this is a ChannelReply::Push (broadcast from pg group)
        // These are forwarded directly to the client WITHOUT going through handle_info
        if let Some(ChannelReply::Push {
            topic,
            event,
            payload,
        }) = raw.decode::<ChannelReply>()
        {
            let room_name = topic.strip_prefix("room:").unwrap_or(&topic);

            // Dispatch based on event name (Phoenix style) rather than trying to
            // infer the variant from payload shape. RoomOutEvent uses #[serde(untagged)]
            // which makes UserJoined and UserLeft indistinguishable since both have
            // the same {nick} shape - serde always picks the first matching variant.
            let server_event: Option<ServerEvent> = match event.as_str() {
                "user_joined" | "presence_sync_response" => {
                    Self::decode_nick_event(&payload).map(|nick| {
                        tracing::debug!(
                            room = %room_name,
                            joined_nick = %nick,
                            my_nick = ?self.nick,
                            "Received UserJoined broadcast, sending to client"
                        );
                        ServerEvent::UserJoined {
                            room: room_name.to_string(),
                            nick,
                        }
                    })
                }
                "user_left" => Self::decode_nick_event(&payload).map(|nick| {
                    tracing::debug!(
                        room = %room_name,
                        leaving_nick = %nick,
                        my_nick = ?self.nick,
                        "Session forwarding UserLeft to client"
                    );
                    ServerEvent::UserLeft {
                        room: room_name.to_string(),
                        nick,
                    }
                }),
                "new_msg" => Self::decode_room_event(&payload).and_then(|re| match re {
                    RoomOutEvent::Message { from, text } => Some(ServerEvent::Message {
                        room: room_name.to_string(),
                        from,
                        text,
                    }),
                    _ => None,
                }),
                "presence_state" => Self::decode_room_event(&payload).and_then(|re| match re {
                    RoomOutEvent::PresenceState { users } => {
                        tracing::debug!(
                            room = %room_name,
                            users = ?users,
                            nick = ?self.nick,
                            "Received PresenceState, sending UserList to client"
                        );
                        Some(ServerEvent::UserList {
                            room: room_name.to_string(),
                            users,
                        })
                    }
                    _ => None,
                }),
                "history" => Self::decode_room_event(&payload).and_then(|re| match re {
                    RoomOutEvent::History { messages } => {
                        tracing::debug!(
                            nick = ?self.nick,
                            room = %room_name,
                            message_count = messages.len(),
                            "Received history push, forwarding to client"
                        );
                        Some(ServerEvent::History {
                            room: room_name.to_string(),
                            messages,
                        })
                    }
                    _ => None,
                }),
                _ => {
                    tracing::debug!(event = %event, room = %room_name, "Unknown channel event");
                    None
                }
            };

            if let Some(evt) = server_event {
                let _ = self.transport.send_event(&evt).await;
            }
            return; // Message handled as broadcast, don't dispatch to handle_info
        }

        // Not a ChannelReply::Push - dispatch to channel handle_info for internal messages
        tracing::debug!(
            nick = ?self.nick,
            data_len = data.len(),
            "Session dispatching non-ChannelReply message to handle_info_any"
        );
        let info_results = self.channels.handle_info_any(data.to_vec().into()).await;
        for (topic, result) in info_results {
            match result {
                ambitious::channel::RawHandleResult::Broadcast { event, payload } => {
                    // Channel wants to broadcast to all members
                    let group = format!("channel:{}", topic);
                    let msg = ChannelReply::Push {
                        topic: topic.clone(),
                        event,
                        payload,
                    };
                    let members = ambitious::dist::pg::get_members(&group);
                    let my_pid = ambitious::current_pid();
                    for member_pid in members {
                        if member_pid != my_pid {
                            let _ = ambitious::send(member_pid, &msg);
                        }
                    }
                }
                ambitious::channel::RawHandleResult::BroadcastFrom { event, payload } => {
                    // Channel wants to broadcast to all members except self
                    let group = format!("channel:{}", topic);
                    let msg = ChannelReply::Push {
                        topic: topic.clone(),
                        event,
                        payload,
                    };
                    let members = ambitious::dist::pg::get_members(&group);
                    let my_pid = ambitious::current_pid();
                    for member_pid in members {
                        if member_pid != my_pid {
                            let _ = ambitious::send(member_pid, &msg);
                        }
                    }
                }
                _ => {
                    // Other results (NoReply, Push, etc.) handled by channel internally
                }
            }
        }
    }

    /// Handle a client command. Returns false if the client should disconnect.
    async fn handle_command(&mut self, cmd: ClientCommand) -> bool {
        match cmd {
            ClientCommand::Nick(nick) => {
                self.handle_nick(nick).await;
            }

            ClientCommand::Join(room_name) => {
                self.handle_join(room_name).await;
            }

            ClientCommand::Leave(room_name) => {
                self.handle_leave(room_name).await;
            }

            ClientCommand::Msg { room, text } => {
                self.handle_msg(room, text).await;
            }

            ClientCommand::ListRooms => {
                let rooms = Registry::list_rooms().await;
                tracing::debug!(room_count = rooms.len(), "Sending room list to client");
                let _ = self
                    .transport
                    .send_event(&ServerEvent::RoomList { rooms })
                    .await;
            }

            ClientCommand::ListUsers(room_name) => {
                self.handle_list_users(room_name).await;
            }

            ClientCommand::Quit => {
                tracing::info!(nick = ?self.nick, "Client quit");
                self.cleanup().await;
                return false;
            }
        }

        true
    }

    /// Handle nick command.
    async fn handle_nick(&mut self, nick: String) {
        if nick.is_empty() || nick.len() > 32 {
            let _ = self
                .transport
                .send_event(&ServerEvent::NickError {
                    reason: "Nickname must be 1-32 characters".to_string(),
                })
                .await;
            return;
        }

        let old_nick = self.nick.clone();
        self.nick = Some(nick.clone());

        tracing::info!(old = ?old_nick, new = %nick, "Nick changed");
        let _ = self
            .transport
            .send_event(&ServerEvent::NickOk { nick })
            .await;
    }

    /// Handle join command using channels.
    async fn handle_join(&mut self, room_name: String) {
        // Must have a nickname first
        let nick = match &self.nick {
            Some(n) => n.clone(),
            None => {
                let _ = self
                    .transport
                    .send_event(&ServerEvent::JoinError {
                        room: room_name,
                        reason: "Set a nickname first with /nick".to_string(),
                    })
                    .await;
                return;
            }
        };

        // Check if already joined
        let topic = format!("room:{}", room_name);
        if self.channels.is_joined(&topic) {
            let _ = self
                .transport
                .send_event(&ServerEvent::JoinError {
                    room: room_name,
                    reason: "Already in this room".to_string(),
                })
                .await;
            return;
        }

        // Get or create the room GenServer first (this registers it globally)
        if let Err(e) = room_supervisor::get_or_create_room(&room_name).await {
            tracing::warn!(room = %room_name, error = ?e, "Failed to create room");
        }

        // Create join payload
        let join_payload = RoomJoin { nick: nick.clone() };
        let payload_bytes = join_payload.encode();

        // Join via channel server
        let msg_ref = format!("join-{}", room_name);
        let reply = self
            .channels
            .handle_join(topic.clone(), payload_bytes, msg_ref)
            .await;

        match reply {
            ChannelReply::JoinOk { .. } => {
                let _ = self
                    .transport
                    .send_event(&ServerEvent::Joined {
                        room: room_name.clone(),
                    })
                    .await;
            }
            ChannelReply::JoinError { reason, .. } => {
                let _ = self
                    .transport
                    .send_event(&ServerEvent::JoinError {
                        room: room_name,
                        reason,
                    })
                    .await;
            }
            _ => {}
        }
    }

    /// Handle leave command.
    async fn handle_leave(&mut self, room_name: String) {
        let topic = format!("room:{}", room_name);

        if !self.channels.is_joined(&topic) {
            let _ = self
                .transport
                .send_event(&ServerEvent::Error {
                    message: format!("Not in room '{}'", room_name),
                })
                .await;
            return;
        }

        // RoomChannel::terminate() broadcasts UserLeft for us
        self.channels.handle_leave(topic).await;
        let _ = self
            .transport
            .send_event(&ServerEvent::Left { room: room_name })
            .await;
    }

    /// Handle message command.
    async fn handle_msg(&mut self, room: String, text: String) {
        let topic = format!("room:{}", room);

        if !self.channels.is_joined(&topic) {
            let _ = self
                .transport
                .send_event(&ServerEvent::Error {
                    message: format!("Not in room '{}'. Join first.", room),
                })
                .await;
            return;
        }

        let nick = match &self.nick {
            Some(n) => n.clone(),
            None => return,
        };

        // Store message in room history
        if let Some(room_pid) = room_supervisor::get_room(&room) {
            cast::<Room>(
                room_pid,
                RoomCast::StoreMessage {
                    from: nick.clone(),
                    text: text.clone(),
                },
            );
        }

        // Broadcast message to all room members
        let event = RoomOutEvent::Message {
            from: nick,
            text: text.clone(),
        };
        let payload_json = serde_json::to_vec(&event).unwrap_or_default();
        let msg = ChannelReply::Push {
            topic: topic.to_string(),
            event: "new_msg".to_string(),
            payload: payload_json,
        };
        let group = format!("channel:{}", topic);
        let members = ambitious::dist::pg::get_members(&group);
        for pid in members {
            let _ = ambitious::send(pid, &msg);
        }
    }

    /// Handle list users command.
    async fn handle_list_users(&mut self, room_name: String) {
        let topic = format!("room:{}", room_name);

        // Use Presence to get the list of users with their metadata
        let presences = match ambitious::presence::Presence::list("chat_presence", &topic).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to get presence list");
                std::collections::HashMap::new()
            }
        };

        // Extract nicknames from presence metadata
        let users: Vec<String> = presences
            .values()
            .flat_map(|state| {
                state.metas.iter().filter_map(|meta| {
                    meta.decode::<crate::channel::UserPresenceMeta>()
                        .map(|m| m.nick)
                })
            })
            .collect();

        let _ = self
            .transport
            .send_event(&ServerEvent::UserList {
                room: room_name,
                users,
            })
            .await;
    }

    /// Decode a nick from a channel event payload (JSON or postcard).
    /// Used for events like user_joined and user_left that carry `{nick}`.
    fn decode_nick_event(payload: &[u8]) -> Option<String> {
        #[derive(Deserialize)]
        struct NickPayload {
            nick: String,
        }
        serde_json::from_slice::<NickPayload>(payload)
            .ok()
            .or_else(|| postcard::from_bytes::<NickPayload>(payload).ok())
            .map(|p| p.nick)
    }

    /// Decode a RoomOutEvent from payload bytes (JSON or postcard).
    fn decode_room_event(payload: &[u8]) -> Option<RoomOutEvent> {
        use ambitious::RawTerm;
        serde_json::from_slice::<RoomOutEvent>(payload)
            .ok()
            .or_else(|| RawTerm::from(payload.to_vec()).decode::<RoomOutEvent>())
    }

    /// Cleanup when disconnecting.
    async fn cleanup(&mut self) {
        // Terminate all channels - RoomChannel::terminate() broadcasts UserLeft for each
        self.channels
            .terminate(ambitious::channel::TerminateReason::Closed)
            .await;

        // Close the transport
        let _ = self.transport.close().await;
    }
}

/// Spawn a session process for a new connection.
pub fn spawn_session(stream: TcpStream) -> Pid {
    ambitious::spawn(move || async move {
        let pid = ambitious::current_pid();
        match Session::new(stream, pid).await {
            Ok(session) => session.run().await,
            Err(e) => {
                tracing::error!(error = %e, "Failed to create session");
            }
        }
    })
}
