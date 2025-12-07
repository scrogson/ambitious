//! Room registry implementation.
//!
//! The registry provides room lookups across the cluster.
//! Rooms are registered globally when users join via Channels,
//! making them visible to all nodes.
//!
//! The struct IS the process state. `init` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::RoomInfo;
use ambitious::dist::global;
use ambitious::gen_server::*;
use ambitious::{Message, Pid, call};
use std::collections::HashMap;
use std::time::Duration;

/// Registry GenServer - the struct IS the state.
pub struct Registry {
    /// Local cache of room name -> PID mappings.
    rooms: HashMap<String, Pid>,
}

// =============================================================================
// Call Messages
// =============================================================================

/// Request to get a room by name.
#[derive(Debug, Clone, Message)]
pub struct GetRoom(pub String);

/// Response with room PID (None if not found).
#[derive(Debug, Clone, Message)]
pub struct RoomResult(pub Option<Pid>);

/// Request to list all rooms.
#[derive(Debug, Clone, Message)]
pub struct ListRooms;

/// Response with list of room info.
#[derive(Debug, Clone, Message)]
pub struct RoomList(pub Vec<RoomInfo>);

// =============================================================================
// Client API
// =============================================================================

impl Registry {
    /// The registered name for the registry process.
    pub const NAME: &'static str = "registry";

    /// Start the registry GenServer.
    pub async fn start() -> Result<Pid, Error> {
        start::<Registry>(()).await
    }

    /// Get a room by name.
    #[allow(dead_code)]
    pub async fn get_room(name: &str) -> Option<Pid> {
        let registry_pid = ambitious::whereis(Self::NAME)?;
        match call::<GetRoom, RoomResult>(
            registry_pid,
            GetRoom(name.to_string()),
            Duration::from_secs(5),
        )
        .await
        {
            Ok(RoomResult(pid)) => pid,
            Err(e) => {
                tracing::error!(error = ?e, "Registry call failed");
                None
            }
        }
    }

    /// List all rooms.
    pub async fn list_rooms() -> Vec<RoomInfo> {
        let Some(registry_pid) = ambitious::whereis(Self::NAME) else {
            return vec![];
        };
        match call::<ListRooms, RoomList>(registry_pid, ListRooms, Duration::from_secs(5)).await {
            Ok(RoomList(rooms)) => rooms,
            Err(e) => {
                tracing::error!(error = ?e, "Registry call failed");
                vec![]
            }
        }
    }
}

// =============================================================================
// GenServer Implementation
// =============================================================================

#[async_trait]
impl GenServer for Registry {
    type Args = ();

    async fn init(_arg: ()) -> Init<Self> {
        tracing::info!("Room registry started");
        Init::Ok(Registry {
            rooms: HashMap::new(),
        })
    }
}

// =============================================================================
// Call Handlers
// =============================================================================

#[call]
impl HandleCall<GetRoom> for Registry {
    type Reply = RoomResult;
    type Output = Reply<RoomResult>;

    async fn handle_call(&mut self, request: GetRoom, _from: From) -> Reply<RoomResult> {
        let name = request.0;

        // Check local cache first
        let mut pid = self.rooms.get(&name).copied();

        // If not in cache, check global registry
        if pid.is_none() {
            let global_name = format!("room:{}", name);
            if let Some(global_pid) = global::whereis(&global_name) {
                // Cache it locally
                self.rooms.insert(name, global_pid);
                pid = Some(global_pid);
            }
        }

        Reply::Ok(RoomResult(pid))
    }
}

#[call]
impl HandleCall<ListRooms> for Registry {
    type Reply = RoomList;
    type Output = Reply<RoomList>;

    async fn handle_call(&mut self, _request: ListRooms, _from: From) -> Reply<RoomList> {
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

        Reply::Ok(RoomList(infos))
    }
}
