//! Room registry implementation using GenServer.
//!
//! The registry provides room lookups across the cluster.
//! Rooms are registered globally when users join via Channels,
//! making them visible to all nodes.

use crate::protocol::RoomInfo;
use serde::{Deserialize, Serialize};
use starlang::dist::global;
use starlang::gen_server::{self, prelude::*};
use std::collections::HashMap;
use std::time::Duration;

/// Registry GenServer implementation.
pub struct Registry;

/// Call requests to Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryCall {
    /// Get a room by name.
    GetRoom(String),
    /// List all rooms with info.
    ListRooms,
}

/// Reply messages from Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryReply {
    /// Room PID (None if not found).
    Room(Option<Pid>),
    /// List of room info.
    Rooms(Vec<RoomInfo>),
}

/// Cast messages to Registry (currently none).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryCast {}

// =============================================================================
// Client API
// =============================================================================

impl Registry {
    /// The registered name for the registry process.
    pub const NAME: &'static str = "registry";

    /// Start the registry GenServer.
    pub async fn start() -> Result<Pid, StartError> {
        gen_server::start::<Registry>(()).await
    }

    /// Get a room by name.
    #[allow(dead_code)]
    pub async fn get_room(name: &str) -> Option<Pid> {
        match Self::call(RegistryCall::GetRoom(name.to_string())).await? {
            RegistryReply::Room(pid) => pid,
            _ => None,
        }
    }

    /// List all rooms.
    pub async fn list_rooms() -> Vec<RoomInfo> {
        match Self::call(RegistryCall::ListRooms).await {
            Some(RegistryReply::Rooms(rooms)) => rooms,
            _ => vec![],
        }
    }

    /// Internal: make a call to the registry.
    async fn call(request: RegistryCall) -> Option<RegistryReply> {
        let registry_pid = starlang::whereis(Self::NAME)?;

        match gen_server::call::<Registry>(registry_pid, request, Duration::from_secs(5)).await {
            Ok(reply) => Some(reply),
            Err(e) => {
                tracing::error!(error = ?e, "Registry call failed");
                None
            }
        }
    }
}

/// Registry state.
pub struct RegistryState {
    /// Local cache of room name -> PID mappings.
    rooms: HashMap<String, Pid>,
}

#[async_trait]
impl GenServer for Registry {
    type State = RegistryState;
    type InitArg = ();
    type Call = RegistryCall;
    type Cast = RegistryCast;
    type Reply = RegistryReply;

    async fn init(_arg: ()) -> InitResult<RegistryState> {
        tracing::info!("Room registry started");
        InitResult::Ok(RegistryState {
            rooms: HashMap::new(),
        })
    }

    async fn handle_call(
        request: RegistryCall,
        _from: From,
        state: &mut RegistryState,
    ) -> CallResult<RegistryState, RegistryReply> {
        match request {
            RegistryCall::GetRoom(name) => {
                // Check local cache first
                let mut pid = state.rooms.get(&name).copied();

                // If not in cache, check global registry
                if pid.is_none() {
                    let global_name = format!("room:{}", name);
                    if let Some(global_pid) = global::whereis(&global_name) {
                        // Cache it locally
                        state.rooms.insert(name, global_pid);
                        pid = Some(global_pid);
                    }
                }

                CallResult::reply(
                    RegistryReply::Room(pid),
                    RegistryState {
                        rooms: state.rooms.clone(),
                    },
                )
            }

            RegistryCall::ListRooms => {
                // Get all globally registered rooms
                let global_rooms = global::registered();
                let room_names: Vec<String> = global_rooms
                    .into_iter()
                    .filter_map(|name| {
                        // Global room names are "room:<name>"
                        name.strip_prefix("room:").map(|s| s.to_string())
                    })
                    .collect();

                let infos: Vec<RoomInfo> = room_names
                    .iter()
                    .map(|name| RoomInfo {
                        name: name.clone(),
                        user_count: 0,
                    })
                    .collect();
                CallResult::reply(
                    RegistryReply::Rooms(infos),
                    RegistryState {
                        rooms: state.rooms.clone(),
                    },
                )
            }
        }
    }

    async fn handle_cast(
        _msg: RegistryCast,
        state: &mut RegistryState,
    ) -> CastResult<RegistryState> {
        CastResult::noreply(RegistryState {
            rooms: state.rooms.clone(),
        })
    }

    async fn handle_info(_msg: Vec<u8>, state: &mut RegistryState) -> InfoResult<RegistryState> {
        CastResult::noreply(RegistryState {
            rooms: state.rooms.clone(),
        })
    }

    async fn handle_continue(
        _arg: ContinueArg,
        state: &mut RegistryState,
    ) -> ContinueResult<RegistryState> {
        CastResult::noreply(RegistryState {
            rooms: state.rooms.clone(),
        })
    }
}
