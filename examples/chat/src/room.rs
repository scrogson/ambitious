//! Chat room implementation using GenServer.
//!
//! Each room is a GenServer that manages its members and broadcasts messages.

use crate::protocol::{ServerEvent, RoomInfo};
use dream_core::Pid;
use dream_gen_server::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, GenServer, InfoResult, InitResult,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Room GenServer implementation.
pub struct Room;

/// Room state.
pub struct RoomState {
    /// Room name.
    pub name: String,
    /// Members: PID -> nickname.
    pub members: HashMap<Pid, String>,
}

/// Initialization argument for Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInit {
    pub name: String,
}


/// Call requests to Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomCall {
    /// Get room info.
    GetInfo,
    /// Get list of members.
    GetMembers,
}


/// Call replies from Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomReply {
    /// Room info.
    Info(RoomInfo),
    /// List of member nicknames.
    Members(Vec<String>),
}


/// Cast messages to Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomCast {
    /// A user joins the room.
    Join { pid: Pid, nick: String },
    /// A user leaves the room.
    Leave { pid: Pid },
    /// Broadcast a message from a user.
    Broadcast { from_pid: Pid, text: String },
    /// Update a user's nickname.
    UpdateNick { pid: Pid, new_nick: String },
}


/// Internal message type for sending events to user sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEvent(pub ServerEvent);


impl GenServer for Room {
    type State = RoomState;
    type InitArg = RoomInit;
    type Call = RoomCall;
    type Cast = RoomCast;
    type Reply = RoomReply;

    fn init(arg: RoomInit) -> InitResult<RoomState> {
        tracing::info!(room = %arg.name, "Room created");
        InitResult::Ok(RoomState {
            name: arg.name,
            members: HashMap::new(),
        })
    }

    fn handle_call(
        request: RoomCall,
        _from: From,
        state: &mut RoomState,
    ) -> CallResult<RoomState, RoomReply> {
        match request {
            RoomCall::GetInfo => {
                let info = RoomInfo {
                    name: state.name.clone(),
                    user_count: state.members.len(),
                };
                // Clone state for the reply
                let new_state = RoomState {
                    name: state.name.clone(),
                    members: state.members.clone(),
                };
                CallResult::Reply(RoomReply::Info(info), new_state)
            }
            RoomCall::GetMembers => {
                let members: Vec<String> = state.members.values().cloned().collect();
                let new_state = RoomState {
                    name: state.name.clone(),
                    members: state.members.clone(),
                };
                CallResult::Reply(RoomReply::Members(members), new_state)
            }
        }
    }

    fn handle_cast(msg: RoomCast, state: &mut RoomState) -> CastResult<RoomState> {
        match msg {
            RoomCast::Join { pid, nick } => {
                tracing::info!(room = %state.name, nick = %nick, "User joined");

                // Notify existing members
                let event = ServerEvent::UserJoined {
                    room: state.name.clone(),
                    nick: nick.clone(),
                };
                broadcast_to_members(state, &event, Some(pid));

                // Add the new member
                state.members.insert(pid, nick);

                CastResult::NoReply(RoomState {
                    name: state.name.clone(),
                    members: state.members.clone(),
                })
            }
            RoomCast::Leave { pid } => {
                if let Some(nick) = state.members.remove(&pid) {
                    tracing::info!(room = %state.name, nick = %nick, "User left");

                    // Notify remaining members
                    let event = ServerEvent::UserLeft {
                        room: state.name.clone(),
                        nick,
                    };
                    broadcast_to_members(state, &event, None);
                }

                CastResult::NoReply(RoomState {
                    name: state.name.clone(),
                    members: state.members.clone(),
                })
            }
            RoomCast::Broadcast { from_pid, text } => {
                if let Some(nick) = state.members.get(&from_pid) {
                    let event = ServerEvent::Message {
                        room: state.name.clone(),
                        from: nick.clone(),
                        text,
                    };
                    broadcast_to_members(state, &event, None);
                }

                CastResult::NoReply(RoomState {
                    name: state.name.clone(),
                    members: state.members.clone(),
                })
            }
            RoomCast::UpdateNick { pid, new_nick } => {
                if let Some(nick) = state.members.get_mut(&pid) {
                    *nick = new_nick;
                }

                CastResult::NoReply(RoomState {
                    name: state.name.clone(),
                    members: state.members.clone(),
                })
            }
        }
    }

    fn handle_info(_msg: Vec<u8>, state: &mut RoomState) -> InfoResult<RoomState> {
        // Handle any raw messages (e.g., monitor DOWN messages)
        InfoResult::NoReply(RoomState {
            name: state.name.clone(),
            members: state.members.clone(),
        })
    }

    fn handle_continue(_arg: ContinueArg, state: &mut RoomState) -> ContinueResult<RoomState> {
        ContinueResult::NoReply(RoomState {
            name: state.name.clone(),
            members: state.members.clone(),
        })
    }
}

/// Broadcast an event to all room members except the excluded PID.
fn broadcast_to_members(state: &RoomState, event: &ServerEvent, exclude: Option<Pid>) {
    let payload = postcard::to_allocvec(&UserEvent(event.clone())).unwrap();
    for &pid in state.members.keys() {
        if Some(pid) != exclude {
            // Send directly to the user session process
            // Note: In a real implementation, we'd use the runtime handle
            // For now, we just log - actual sending happens via the registry
            tracing::debug!(pid = ?pid, event = ?event, "Would send to user");
            let _ = &payload; // Suppress unused warning
        }
    }
}
