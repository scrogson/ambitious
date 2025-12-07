//! Room registry implementation using v3 enum-based dispatch.
//!
//! The registry provides room lookups across the cluster.
//! Rooms are registered globally when users join via Channels,
//! making them visible to all nodes.
//!
//! The struct IS the process state. `init` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::RoomInfo;
use ambitious::dist::global;
use ambitious::gen_server::v3::{
    async_trait, call, start, Error, From, GenServer, Init, Reply, Status,
};
use ambitious::message::Message;
use ambitious::core::{DecodeError, Pid};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Registry GenServer - the struct IS the state.
pub struct Registry {
    /// Local cache of room name -> PID mappings.
    rooms: HashMap<String, Pid>,
}

// =============================================================================
// Call Message Enum
// =============================================================================

/// All call (request/response) messages for Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryCall {
    /// Request to get a room by name.
    GetRoom(String),
    /// Request to list all rooms.
    ListRooms,
}

impl Message for RegistryCall {
    fn tag() -> &'static str {
        "RegistryCall"
    }
    fn encode_local(&self) -> Vec<u8> {
        ambitious::message::encode_with_tag(
            Self::tag(),
            &ambitious::message::encode_payload(self),
        )
    }
    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
        ambitious::message::decode_payload(bytes)
    }
    fn encode_remote(&self) -> Vec<u8> {
        self.encode_local()
    }
    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_local(bytes)
    }
}

// =============================================================================
// Reply Type
// =============================================================================

/// Reply type for Registry calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryReply {
    /// Response with room PID (None if not found).
    RoomResult(Option<Pid>),
    /// Response with list of room info.
    RoomList(Vec<RoomInfo>),
}

impl Message for RegistryReply {
    fn tag() -> &'static str {
        "RegistryReply"
    }
    fn encode_local(&self) -> Vec<u8> {
        ambitious::message::encode_with_tag(
            Self::tag(),
            &ambitious::message::encode_payload(self),
        )
    }
    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
        ambitious::message::decode_payload(bytes)
    }
    fn encode_remote(&self) -> Vec<u8> {
        self.encode_local()
    }
    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_local(bytes)
    }
}

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
        match call::<Registry, RegistryReply>(
            registry_pid,
            RegistryCall::GetRoom(name.to_string()),
            Duration::from_secs(5),
        )
        .await
        {
            Ok(RegistryReply::RoomResult(pid)) => pid,
            Ok(_) => None,
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
        match call::<Registry, RegistryReply>(
            registry_pid,
            RegistryCall::ListRooms,
            Duration::from_secs(5),
        )
        .await
        {
            Ok(RegistryReply::RoomList(rooms)) => rooms,
            Ok(_) => vec![],
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
    type Call = RegistryCall;
    type Cast = ();
    type Info = ();
    type Reply = RegistryReply;

    async fn init(_arg: ()) -> Init<Self> {
        tracing::info!("Room registry started");
        Init::Ok(Registry {
            rooms: HashMap::new(),
        })
    }

    async fn handle_call(&mut self, msg: RegistryCall, _from: From) -> Reply<RegistryReply> {
        match msg {
            RegistryCall::GetRoom(name) => {
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

                Reply::Ok(RegistryReply::RoomResult(pid))
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

                Reply::Ok(RegistryReply::RoomList(infos))
            }
        }
    }

    async fn handle_cast(&mut self, _msg: ()) -> Status {
        Status::Ok
    }

    async fn handle_info(&mut self, _msg: ()) -> Status {
        Status::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_name() {
        assert_eq!(Registry::NAME, "registry");
    }
}
