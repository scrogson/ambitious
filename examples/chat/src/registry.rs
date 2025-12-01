//! Room registry GenServer.
//!
//! The registry manages all chat rooms, creating them on demand
//! and providing lookups by name.

use crate::protocol::RoomInfo;
use crate::room::{Room, RoomInit};
use dream_core::Pid;
use dream_gen_server::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, GenServer, InfoResult, InitResult,
};
use dream_process::RuntimeHandle;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Room registry GenServer.
pub struct Registry;

/// Registry state.
pub struct RegistryState {
    /// Room name -> Room PID.
    rooms: HashMap<String, Pid>,
    /// Runtime handle for spawning rooms.
    handle: RuntimeHandle,
}

/// Initialization argument for Registry.
#[derive(Clone)]
pub struct RegistryInit {
    pub handle: RuntimeHandle,
}

// Custom serialization for RegistryInit since RuntimeHandle isn't serializable
impl Serialize for RegistryInit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as unit - we'll reconstruct the handle another way
        serializer.serialize_unit()
    }
}

impl<'de> Deserialize<'de> for RegistryInit {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // This won't actually be called since we pass the init directly
        panic!("RegistryInit cannot be deserialized")
    }
}


/// Call requests to Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryCall {
    /// Get a room by name.
    GetRoom(String),
    /// Get or create a room by name.
    GetOrCreateRoom(String),
    /// List all rooms with info.
    ListRooms,
}


/// Call replies from Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryReply {
    /// Room PID (None if not found).
    Room(Option<Pid>),
    /// List of room info.
    Rooms(Vec<RoomInfo>),
}


/// Cast messages to Registry (currently unused).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryCast {
    /// Remove a room (when it's empty).
    RemoveRoom(String),
}


impl GenServer for Registry {
    type State = RegistryState;
    type InitArg = RegistryInit;
    type Call = RegistryCall;
    type Cast = RegistryCast;
    type Reply = RegistryReply;

    fn init(arg: RegistryInit) -> InitResult<RegistryState> {
        tracing::info!("Room registry started");
        InitResult::Ok(RegistryState {
            rooms: HashMap::new(),
            handle: arg.handle,
        })
    }

    fn handle_call(
        request: RegistryCall,
        _from: From,
        state: &mut RegistryState,
    ) -> CallResult<RegistryState, RegistryReply> {
        match request {
            RegistryCall::GetRoom(name) => {
                let pid = state.rooms.get(&name).copied();
                let new_state = RegistryState {
                    rooms: state.rooms.clone(),
                    handle: state.handle.clone(),
                };
                CallResult::Reply(RegistryReply::Room(pid), new_state)
            }

            RegistryCall::GetOrCreateRoom(name) => {
                // Check if room exists
                if let Some(&pid) = state.rooms.get(&name) {
                    let new_state = RegistryState {
                        rooms: state.rooms.clone(),
                        handle: state.handle.clone(),
                    };
                    return CallResult::Reply(RegistryReply::Room(Some(pid)), new_state);
                }

                // Create new room - we need to do this synchronously
                // In a real implementation, we'd use async initialization
                let handle = state.handle.clone();
                let room_name = name.clone();

                // Spawn the room synchronously using a blocking approach
                // Note: This is a simplification. In production, you'd want
                // to handle this more elegantly with async init.
                let room_pid = std::thread::scope(|s| {
                    s.spawn(|| {
                        let rt = tokio::runtime::Handle::current();
                        rt.block_on(async {
                            dream_gen_server::start::<Room>(
                                &handle,
                                RoomInit { name: room_name },
                            )
                            .await
                            .ok()
                        })
                    })
                    .join()
                    .ok()
                    .flatten()
                });

                if let Some(pid) = room_pid {
                    state.rooms.insert(name, pid);
                    let new_state = RegistryState {
                        rooms: state.rooms.clone(),
                        handle: state.handle.clone(),
                    };
                    CallResult::Reply(RegistryReply::Room(Some(pid)), new_state)
                } else {
                    let new_state = RegistryState {
                        rooms: state.rooms.clone(),
                        handle: state.handle.clone(),
                    };
                    CallResult::Reply(RegistryReply::Room(None), new_state)
                }
            }

            RegistryCall::ListRooms => {
                // Get info from each room
                // Note: We just return basic info without querying rooms
                // since we can't call GenServer from here without a Context
                let infos: Vec<RoomInfo> = state
                    .rooms
                    .keys()
                    .map(|name| RoomInfo {
                        name: name.clone(),
                        user_count: 0, // Would need to query room
                    })
                    .collect();

                let new_state = RegistryState {
                    rooms: state.rooms.clone(),
                    handle: state.handle.clone(),
                };
                CallResult::Reply(RegistryReply::Rooms(infos), new_state)
            }
        }
    }

    fn handle_cast(msg: RegistryCast, state: &mut RegistryState) -> CastResult<RegistryState> {
        match msg {
            RegistryCast::RemoveRoom(name) => {
                state.rooms.remove(&name);
                tracing::info!(room = %name, "Room removed from registry");
                CastResult::NoReply(RegistryState {
                    rooms: state.rooms.clone(),
                    handle: state.handle.clone(),
                })
            }
        }
    }

    fn handle_info(_msg: Vec<u8>, state: &mut RegistryState) -> InfoResult<RegistryState> {
        InfoResult::NoReply(RegistryState {
            rooms: state.rooms.clone(),
            handle: state.handle.clone(),
        })
    }

    fn handle_continue(
        _arg: ContinueArg,
        state: &mut RegistryState,
    ) -> ContinueResult<RegistryState> {
        ContinueResult::NoReply(RegistryState {
            rooms: state.rooms.clone(),
            handle: state.handle.clone(),
        })
    }
}

/// Start the registry GenServer.
pub async fn start_registry(handle: &RuntimeHandle) -> Result<Pid, dream_gen_server::StartError> {
    dream_gen_server::start::<Registry>(handle, RegistryInit { handle: handle.clone() }).await
}
