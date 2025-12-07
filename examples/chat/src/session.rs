//! User session process using Channels.
//!
//! Each connected client gets a session process that:
//! - Reads commands from the TCP socket
//! - Sends events back to the client
//! - Uses ChannelServer for room management

use crate::channel::{RoomChannel, RoomJoin, RoomOutEvent};
use crate::protocol::{ClientCommand, ServerEvent, frame_message, parse_frame};
use crate::registry::Registry;
use crate::room::{Room, RoomCast};
use crate::room_supervisor;
use ambitious::channel::{ChannelReply, ChannelServer, ChannelServerBuilder};
use ambitious::gen_server::cast;
use ambitious::{Pid, RawTerm, Term};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// User session state.
pub struct Session {
    /// TCP stream for this client.
    stream: Arc<Mutex<TcpStream>>,
    /// User's nickname.
    nick: Option<String>,
    /// Channel server for managing room connections.
    channels: ChannelServer,
}

impl Session {
    pub fn new(stream: TcpStream, pid: Pid) -> Self {
        // Build channel server with RoomChannel handler
        let channels = ChannelServerBuilder::new()
            .channel::<RoomChannel>()
            .build(pid);

        Self {
            stream: Arc::new(Mutex::new(stream)),
            nick: None,
            channels,
        }
    }

    /// Run the session, processing commands until disconnect.
    pub async fn run(mut self) {
        // Send welcome message
        self.send_event(ServerEvent::Welcome {
            message: "Welcome to Ambitious Chat! Use /nick <name> to set your nickname."
                .to_string(),
        })
        .await;

        let mut buf = vec![0u8; 4096];
        let mut pending = Vec::new();

        loop {
            // Try to parse any pending data first
            while let Some((cmd, consumed)) = parse_frame::<ClientCommand>(&pending) {
                pending.drain(..consumed);
                if !self.handle_command(cmd).await {
                    return; // Client quit
                }
            }

            // First, drain any pending process messages (non-blocking)
            while let Some(data) = ambitious::try_recv() {
                self.handle_channel_message(&data).await;
            }

            // Now wait for either TCP data or process messages
            let stream = self.stream.clone();
            let mut guard = stream.lock().await;

            tokio::select! {
                biased; // Prefer process messages over TCP

                // Check for messages from channels (broadcasts, pushes)
                msg = ambitious::recv_timeout(Duration::from_millis(100)) => {
                    drop(guard); // Release lock before processing
                    if let Ok(Some(data)) = msg {
                        self.handle_channel_message(&data).await;
                    }
                }

                result = guard.read(&mut buf) => {
                    match result {
                        Ok(0) => {
                            // Connection closed
                            tracing::info!(nick = ?self.nick, "Client disconnected");
                            self.cleanup().await;
                            return;
                        }
                        Ok(n) => {
                            pending.extend_from_slice(&buf[..n]);
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
        let raw = RawTerm::from(data.to_vec());

        // First, check if this is a ChannelReply::Push (broadcast from pg group)
        // These are forwarded directly to the client WITHOUT going through handle_info
        if let Some(ChannelReply::Push {
            topic,
            event: _,
            payload,
        }) = raw.decode::<ChannelReply>()
        {
            // Decode the room event and forward to client
            let payload_raw = RawTerm::from(payload);
            if let Some(room_event) = payload_raw.decode::<RoomOutEvent>() {
                let room_name = topic.strip_prefix("room:").unwrap_or(&topic);
                match room_event {
                    RoomOutEvent::UserJoined { nick } => {
                        tracing::debug!(
                            room = %room_name,
                            joined_nick = %nick,
                            my_nick = ?self.nick,
                            "Received UserJoined broadcast, sending to client"
                        );
                        self.send_event(ServerEvent::UserJoined {
                            room: room_name.to_string(),
                            nick,
                        })
                        .await;
                    }
                    RoomOutEvent::UserLeft { nick } => {
                        self.send_event(ServerEvent::UserLeft {
                            room: room_name.to_string(),
                            nick,
                        })
                        .await;
                    }
                    RoomOutEvent::Message { from, text } => {
                        self.send_event(ServerEvent::Message {
                            room: room_name.to_string(),
                            from,
                            text,
                        })
                        .await;
                    }
                    RoomOutEvent::PresenceState { users } => {
                        tracing::debug!(
                            room = %room_name,
                            users = ?users,
                            nick = ?self.nick,
                            "Received PresenceState broadcast, sending UserList to client"
                        );
                        self.send_event(ServerEvent::UserList {
                            room: room_name.to_string(),
                            users,
                        })
                        .await;
                    }
                    RoomOutEvent::PresenceSyncRequest { .. } => {
                        // These should go through handle_info, not be broadcast
                        // But if we receive one, ignore it here
                    }
                    RoomOutEvent::PresenceSyncResponse { nick } => {
                        // An existing member announced themselves
                        tracing::debug!(
                            room = %room_name,
                            nick = %nick,
                            "Received presence sync response, sending UserJoined to client"
                        );
                        self.send_event(ServerEvent::UserJoined {
                            room: room_name.to_string(),
                            nick,
                        })
                        .await;
                    }
                    RoomOutEvent::History { messages } => {
                        // History pushed to this client specifically
                        tracing::debug!(
                            nick = ?self.nick,
                            room = %room_name,
                            message_count = messages.len(),
                            "Received history push, forwarding to client"
                        );
                        self.send_event(ServerEvent::History {
                            room: room_name.to_string(),
                            messages,
                        })
                        .await;
                    }
                }
            }
            return; // Message handled as broadcast, don't dispatch to handle_info
        }

        // Not a ChannelReply::Push - dispatch to channel handle_info for internal messages
        // (e.g., ChannelInfo::AfterJoin, PresenceMessage::Delta, etc.)
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
                self.send_event(ServerEvent::RoomList { rooms }).await;
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
            self.send_event(ServerEvent::NickError {
                reason: "Nickname must be 1-32 characters".to_string(),
            })
            .await;
            return;
        }

        let old_nick = self.nick.clone();
        self.nick = Some(nick.clone());

        tracing::info!(old = ?old_nick, new = %nick, "Nick changed");
        self.send_event(ServerEvent::NickOk { nick }).await;
    }

    /// Handle join command using channels.
    async fn handle_join(&mut self, room_name: String) {
        // Must have a nickname first
        let nick = match &self.nick {
            Some(n) => n.clone(),
            None => {
                self.send_event(ServerEvent::JoinError {
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
            self.send_event(ServerEvent::JoinError {
                room: room_name,
                reason: "Already in this room".to_string(),
            })
            .await;
            return;
        }

        // Get or create the room GenServer first (this registers it globally)
        // The room is created here so it exists before the channel join
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
                // Note: History is sent via the channel's AfterJoin handler
                // to ensure it only goes to the joining user (not duplicated here)

                self.send_event(ServerEvent::Joined {
                    room: room_name.clone(),
                })
                .await;

                // RoomChannel::join() sends :after_join which will:
                // - Broadcast UserJoined to other members
                // - Push history to the joining user
                // - Schedule PushPresenceState to send full user list after sync
                // These are handled when the mailbox messages arrive in handle_channel_message
            }
            ChannelReply::JoinError { reason, .. } => {
                self.send_event(ServerEvent::JoinError {
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
            self.send_event(ServerEvent::Error {
                message: format!("Not in room '{}'", room_name),
            })
            .await;
            return;
        }

        // RoomChannel::terminate() broadcasts UserLeft for us
        self.channels.handle_leave(topic).await;
        self.send_event(ServerEvent::Left { room: room_name }).await;
    }

    /// Handle message command.
    async fn handle_msg(&mut self, room: String, text: String) {
        let topic = format!("room:{}", room);

        if !self.channels.is_joined(&topic) {
            self.send_event(ServerEvent::Error {
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
        let msg = ChannelReply::Push {
            topic: topic.to_string(),
            event: "new_msg".to_string(),
            payload: event.encode(),
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

        self.send_event(ServerEvent::UserList {
            room: room_name,
            users,
        })
        .await;
    }

    /// Send an event to the client.
    async fn send_event(&self, event: ServerEvent) {
        let frame = frame_message(&event);
        let stream = self.stream.clone();
        let mut guard = stream.lock().await;
        if let Err(e) = guard.write_all(&frame).await {
            tracing::error!(error = %e, "Failed to send event");
        }
    }

    /// Cleanup when disconnecting.
    async fn cleanup(&mut self) {
        // Terminate all channels - RoomChannel::terminate() broadcasts UserLeft for each
        self.channels
            .terminate(ambitious::channel::TerminateReason::Closed)
            .await;
    }
}

/// Spawn a session process for a new connection.
pub fn spawn_session(stream: TcpStream) -> Pid {
    ambitious::spawn(move || async move {
        let pid = ambitious::current_pid();
        let session = Session::new(stream, pid);
        session.run().await;
    })
}
