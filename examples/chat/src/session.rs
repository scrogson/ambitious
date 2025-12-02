//! User session process using Channels.
//!
//! Each connected client gets a session process that:
//! - Reads commands from the TCP socket
//! - Sends events back to the client
//! - Uses ChannelServer for room management

use crate::channel::{JoinPayload, RoomChannel, RoomOutEvent};
use crate::protocol::{frame_message, parse_frame, ClientCommand, ServerEvent};
use crate::registry::Registry;
use dream::channel::{ChannelReply, ChannelServer, ChannelServerBuilder};
use dream::Pid;
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
            message: "Welcome to DREAM Chat! Use /nick <name> to set your nickname.".to_string(),
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

            // Read more data from socket or receive channel messages
            let stream = self.stream.clone();
            let mut guard = stream.lock().await;

            tokio::select! {
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
                // Check for messages from channels (broadcasts, pushes)
                msg = dream::recv_timeout(Duration::from_millis(10)) => {
                    drop(guard); // Release lock before processing
                    if let Ok(Some(data)) = msg {
                        self.handle_channel_message(&data).await;
                    }
                }
            }
        }
    }

    /// Handle incoming channel messages (broadcasts from other users).
    async fn handle_channel_message(&mut self, data: &[u8]) {
        // Try to decode as ChannelReply (push from channel)
        if let Ok(reply) = postcard::from_bytes::<ChannelReply>(data) {
            match reply {
                ChannelReply::Push {
                    topic,
                    event,
                    payload,
                } => {
                    // Decode the room event
                    if let Ok(room_event) = postcard::from_bytes::<RoomOutEvent>(&payload) {
                        let room_name = topic.strip_prefix("room:").unwrap_or(&topic);
                        let server_event = match room_event {
                            RoomOutEvent::UserJoined { nick } => ServerEvent::UserJoined {
                                room: room_name.to_string(),
                                nick,
                            },
                            RoomOutEvent::UserLeft { nick } => ServerEvent::UserLeft {
                                room: room_name.to_string(),
                                nick,
                            },
                            RoomOutEvent::Message { from, text } => ServerEvent::Message {
                                room: room_name.to_string(),
                                from,
                                text,
                            },
                            RoomOutEvent::PresenceState { users } => ServerEvent::UserList {
                                room: room_name.to_string(),
                                users,
                            },
                        };
                        self.send_event(server_event).await;
                    } else {
                        tracing::warn!(event = %event, "Failed to decode room event");
                    }
                }
                _ => {
                    // Other replies are handled inline
                }
            }
        }
        // Also try legacy UserEvent format for backwards compatibility
        else if let Ok(event) = postcard::from_bytes::<crate::room::UserEvent>(data) {
            self.send_event(event.0).await;
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

        // Create join payload
        let join_payload = JoinPayload { nick: nick.clone() };
        let payload_bytes = match postcard::to_allocvec(&join_payload) {
            Ok(b) => b,
            Err(_) => {
                self.send_event(ServerEvent::JoinError {
                    room: room_name,
                    reason: "Internal error".to_string(),
                })
                .await;
                return;
            }
        };

        // Join via channel server
        let msg_ref = format!("join-{}", room_name);
        let reply = self
            .channels
            .handle_join(topic.clone(), payload_bytes, msg_ref)
            .await;

        match reply {
            ChannelReply::JoinOk { .. } => {
                // Broadcast that we joined to other members
                self.broadcast_join(&topic, &nick).await;
                self.send_event(ServerEvent::Joined { room: room_name }).await;
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

    /// Broadcast a join event to the room.
    async fn broadcast_join(&self, topic: &str, nick: &str) {
        let event = RoomOutEvent::UserJoined {
            nick: nick.to_string(),
        };
        if let Ok(payload) = postcard::to_allocvec(&event) {
            let msg = ChannelReply::Push {
                topic: topic.to_string(),
                event: "user_joined".to_string(),
                payload,
            };
            if let Ok(bytes) = postcard::to_allocvec(&msg) {
                let group = format!("channel:{}", topic);
                let members = dream::dist::pg::get_members(&group);
                let my_pid = dream::current_pid();
                for pid in members {
                    if pid != my_pid {
                        let _ = dream::send_raw(pid, bytes.clone());
                    }
                }
            }
        }
    }

    /// Broadcast a leave event to the room.
    async fn broadcast_leave(&self, topic: &str, nick: &str) {
        let event = RoomOutEvent::UserLeft {
            nick: nick.to_string(),
        };
        if let Ok(payload) = postcard::to_allocvec(&event) {
            let msg = ChannelReply::Push {
                topic: topic.to_string(),
                event: "user_left".to_string(),
                payload,
            };
            if let Ok(bytes) = postcard::to_allocvec(&msg) {
                let group = format!("channel:{}", topic);
                let members = dream::dist::pg::get_members(&group);
                for pid in members {
                    let _ = dream::send_raw(pid, bytes.clone());
                }
            }
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

        // Broadcast leave before actually leaving
        if let Some(nick) = &self.nick {
            self.broadcast_leave(&topic, nick).await;
        }

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

        // Broadcast message to all room members
        let event = RoomOutEvent::Message {
            from: nick,
            text,
        };
        if let Ok(payload) = postcard::to_allocvec(&event) {
            let msg = ChannelReply::Push {
                topic: topic.to_string(),
                event: "new_msg".to_string(),
                payload,
            };
            if let Ok(bytes) = postcard::to_allocvec(&msg) {
                let group = format!("channel:{}", topic);
                let members = dream::dist::pg::get_members(&group);
                for pid in members {
                    let _ = dream::send_raw(pid, bytes.clone());
                }
            }
        }
    }

    /// Handle list users command.
    async fn handle_list_users(&mut self, room_name: String) {
        let topic = format!("room:{}", room_name);

        // Use Presence to get the list of users with their metadata
        let presences = dream::presence::list(&topic);

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
        // Get joined topics before terminating
        let topics: Vec<String> = self.channels.joined_topics().iter().map(|s| s.to_string()).collect();

        // Broadcast leave for each room
        if let Some(nick) = &self.nick {
            for topic in &topics {
                self.broadcast_leave(topic, nick).await;
            }
        }

        // Terminate all channels
        self.channels
            .terminate(dream::channel::TerminateReason::Closed)
            .await;
    }
}

/// Spawn a session process for a new connection.
pub fn spawn_session(stream: TcpStream) -> Pid {
    dream::spawn(move || async move {
        let pid = dream::current_pid();
        let session = Session::new(stream, pid);
        session.run().await;
    })
}
