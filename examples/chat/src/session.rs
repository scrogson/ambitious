//! User session process.
//!
//! Each connected client gets a session process that:
//! - Reads commands from the TCP socket
//! - Sends events back to the client
//! - Manages room memberships

use crate::protocol::{frame_message, parse_frame, ClientCommand, ServerEvent};
use crate::registry::{RegistryCall, RegistryReply, Registry};
use crate::room::{RoomCall, RoomCast, RoomReply, Room};
use dream_core::Pid;
use dream_process::RuntimeHandle;
use dream_runtime::Context;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// User session state.
pub struct Session {
    /// The runtime handle.
    handle: RuntimeHandle,
    /// TCP stream for this client.
    stream: Arc<Mutex<TcpStream>>,
    /// User's nickname.
    nick: Option<String>,
    /// Rooms the user has joined.
    rooms: HashSet<String>,
    /// Registry PID.
    registry_pid: Pid,
}

impl Session {
    pub fn new(handle: RuntimeHandle, stream: TcpStream, registry_pid: Pid) -> Self {
        Self {
            handle,
            stream: Arc::new(Mutex::new(stream)),
            nick: None,
            rooms: HashSet::new(),
            registry_pid,
        }
    }

    /// Run the session, processing commands until disconnect.
    pub async fn run(mut self, ctx: &mut Context) {
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
                if !self.handle_command(cmd, ctx).await {
                    return; // Client quit
                }
            }

            // Read more data from socket
            let stream = self.stream.clone();
            let mut guard = stream.lock().await;

            tokio::select! {
                result = guard.read(&mut buf) => {
                    match result {
                        Ok(0) => {
                            // Connection closed
                            tracing::info!(nick = ?self.nick, "Client disconnected");
                            self.cleanup(ctx).await;
                            return;
                        }
                        Ok(n) => {
                            pending.extend_from_slice(&buf[..n]);
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Read error");
                            self.cleanup(ctx).await;
                            return;
                        }
                    }
                }
                // Check for messages from other processes
                msg = ctx.recv_timeout(Duration::from_millis(10)) => {
                    drop(guard); // Release lock before processing
                    if let Ok(Some(data)) = msg {
                        // Try to decode as a UserEvent
                        if let Ok(event) = postcard::from_bytes::<crate::room::UserEvent>(&data) {
                            self.send_event(event.0).await;
                        }
                    }
                }
            }
        }
    }

    /// Handle a client command. Returns false if the client should disconnect.
    async fn handle_command(&mut self, cmd: ClientCommand, ctx: &mut Context) -> bool {
        match cmd {
            ClientCommand::Nick(nick) => {
                if nick.is_empty() || nick.len() > 32 {
                    self.send_event(ServerEvent::NickError {
                        reason: "Nickname must be 1-32 characters".to_string(),
                    })
                    .await;
                } else {
                    let old_nick = self.nick.clone();
                    self.nick = Some(nick.clone());

                    // Update nick in all joined rooms
                    for room_name in &self.rooms {
                        if let Some(room_pid) = self.get_room_pid(room_name, ctx).await {
                            let _ = dream_gen_server::cast::<Room>(
                                &self.handle,
                                room_pid,
                                RoomCast::UpdateNick {
                                    pid: ctx.pid(),
                                    new_nick: nick.clone(),
                                },
                            );
                        }
                    }

                    tracing::info!(old = ?old_nick, new = %nick, "Nick changed");
                    self.send_event(ServerEvent::NickOk { nick }).await;
                }
            }

            ClientCommand::Join(room_name) => {
                if self.nick.is_none() {
                    self.send_event(ServerEvent::JoinError {
                        room: room_name,
                        reason: "Set a nickname first with /nick".to_string(),
                    })
                    .await;
                    return true;
                }

                if self.rooms.contains(&room_name) {
                    self.send_event(ServerEvent::JoinError {
                        room: room_name,
                        reason: "Already in this room".to_string(),
                    })
                    .await;
                    return true;
                }

                // Get or create the room via registry
                match self.get_or_create_room(&room_name, ctx).await {
                    Some(room_pid) => {
                        // Join the room
                        let _ = dream_gen_server::cast::<Room>(
                            &self.handle,
                            room_pid,
                            RoomCast::Join {
                                pid: ctx.pid(),
                                nick: self.nick.clone().unwrap(),
                            },
                        );

                        self.rooms.insert(room_name.clone());
                        self.send_event(ServerEvent::Joined { room: room_name }).await;
                    }
                    None => {
                        self.send_event(ServerEvent::JoinError {
                            room: room_name,
                            reason: "Failed to create room".to_string(),
                        })
                        .await;
                    }
                }
            }

            ClientCommand::Leave(room_name) => {
                if !self.rooms.contains(&room_name) {
                    self.send_event(ServerEvent::Error {
                        message: format!("Not in room '{}'", room_name),
                    })
                    .await;
                    return true;
                }

                if let Some(room_pid) = self.get_room_pid(&room_name, ctx).await {
                    let _ = dream_gen_server::cast::<Room>(
                        &self.handle,
                        room_pid,
                        RoomCast::Leave { pid: ctx.pid() },
                    );
                }

                self.rooms.remove(&room_name);
                self.send_event(ServerEvent::Left { room: room_name }).await;
            }

            ClientCommand::Msg { room, text } => {
                if !self.rooms.contains(&room) {
                    self.send_event(ServerEvent::Error {
                        message: format!("Not in room '{}'. Join first.", room),
                    })
                    .await;
                    return true;
                }

                if let Some(room_pid) = self.get_room_pid(&room, ctx).await {
                    let _ = dream_gen_server::cast::<Room>(
                        &self.handle,
                        room_pid,
                        RoomCast::Broadcast {
                            from_pid: ctx.pid(),
                            text,
                        },
                    );
                }
            }

            ClientCommand::ListRooms => {
                match self.call_registry(RegistryCall::ListRooms, ctx).await {
                    Some(RegistryReply::Rooms(rooms)) => {
                        self.send_event(ServerEvent::RoomList { rooms }).await;
                    }
                    _ => {
                        self.send_event(ServerEvent::RoomList { rooms: vec![] }).await;
                    }
                }
            }

            ClientCommand::ListUsers(room_name) => {
                if let Some(room_pid) = self.get_room_pid(&room_name, ctx).await {
                    match dream_gen_server::call::<Room>(
                        &self.handle,
                        ctx,
                        room_pid,
                        RoomCall::GetMembers,
                        Duration::from_secs(5),
                    )
                    .await
                    {
                        Ok(RoomReply::Members(users)) => {
                            self.send_event(ServerEvent::UserList {
                                room: room_name,
                                users,
                            })
                            .await;
                        }
                        _ => {
                            self.send_event(ServerEvent::Error {
                                message: "Failed to get user list".to_string(),
                            })
                            .await;
                        }
                    }
                } else {
                    self.send_event(ServerEvent::Error {
                        message: format!("Room '{}' not found", room_name),
                    })
                    .await;
                }
            }

            ClientCommand::Quit => {
                tracing::info!(nick = ?self.nick, "Client quit");
                self.cleanup(ctx).await;
                return false;
            }
        }

        true
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

    /// Get room PID from registry.
    async fn get_room_pid(&self, room_name: &str, ctx: &mut Context) -> Option<Pid> {
        match self
            .call_registry(RegistryCall::GetRoom(room_name.to_string()), ctx)
            .await
        {
            Some(RegistryReply::Room(pid)) => pid,
            _ => None,
        }
    }

    /// Get or create a room via registry.
    async fn get_or_create_room(&self, room_name: &str, ctx: &mut Context) -> Option<Pid> {
        match self
            .call_registry(RegistryCall::GetOrCreateRoom(room_name.to_string()), ctx)
            .await
        {
            Some(RegistryReply::Room(pid)) => pid,
            _ => None,
        }
    }

    /// Call the registry GenServer.
    async fn call_registry(&self, request: RegistryCall, ctx: &mut Context) -> Option<RegistryReply> {
        dream_gen_server::call::<Registry>(
            &self.handle,
            ctx,
            self.registry_pid,
            request,
            Duration::from_secs(5),
        )
        .await
        .ok()
    }

    /// Cleanup when disconnecting.
    async fn cleanup(&mut self, ctx: &mut Context) {
        // Collect room names first to avoid borrow issues
        let rooms: Vec<String> = self.rooms.drain().collect();

        // Leave all rooms
        for room_name in rooms {
            if let Some(room_pid) = self.get_room_pid(&room_name, ctx).await {
                let _ = dream_gen_server::cast::<Room>(
                    &self.handle,
                    room_pid,
                    RoomCast::Leave { pid: ctx.pid() },
                );
            }
        }
    }
}

/// Spawn a session process for a new connection.
pub fn spawn_session(
    handle: &RuntimeHandle,
    stream: TcpStream,
    registry_pid: Pid,
) -> Pid {
    let handle_clone = handle.clone();
    handle.spawn(move |ctx| async move {
        let mut ctx = ctx;
        let session = Session::new(handle_clone, stream, registry_pid);
        session.run(&mut ctx).await;
    })
}
