//! Room registry implementation using GenServer v2 API.
//!
//! The registry provides room lookups across the cluster.
//! Rooms are registered globally when users join via Channels,
//! making them visible to all nodes.
//!
//! In v2, the struct IS the process state. `init` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::RoomInfo;
use ambitious::Pid;
use ambitious::core::DecodeError;
use ambitious::dist::global;
use ambitious::gen_server::v2::*;
use ambitious::message::{Message, decode_payload, decode_tag, encode_payload, encode_with_tag};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Registry GenServer - the struct IS the state.
pub struct Registry {
    /// Local cache of room name -> PID mappings.
    rooms: HashMap<String, Pid>,
}

/// Call requests to Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryCall {
    /// Get a room by name.
    GetRoom(String),
    /// List all rooms with info.
    ListRooms,
}

impl Message for RegistryCall {
    fn tag() -> &'static str {
        "RegistryCall"
    }

    fn encode_local(&self) -> Vec<u8> {
        let payload = encode_payload(self);
        encode_with_tag(Self::tag(), &payload)
    }

    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode_payload(bytes)
    }

    fn encode_remote(&self) -> Vec<u8> {
        self.encode_local()
    }

    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_local(bytes)
    }
}

/// Reply messages from Registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegistryReply {
    /// Room PID (None if not found).
    Room(Option<Pid>),
    /// List of room info.
    Rooms(Vec<RoomInfo>),
}

impl Message for RegistryReply {
    fn tag() -> &'static str {
        "RegistryReply"
    }

    fn encode_local(&self) -> Vec<u8> {
        let payload = encode_payload(self);
        encode_with_tag(Self::tag(), &payload)
    }

    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode_payload(bytes)
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
        match Self::do_call(RegistryCall::GetRoom(name.to_string())).await? {
            RegistryReply::Room(pid) => pid,
            _ => None,
        }
    }

    /// List all rooms.
    pub async fn list_rooms() -> Vec<RoomInfo> {
        match Self::do_call(RegistryCall::ListRooms).await {
            Some(RegistryReply::Rooms(rooms)) => rooms,
            _ => vec![],
        }
    }

    /// Internal: make a call to the registry.
    async fn do_call(request: RegistryCall) -> Option<RegistryReply> {
        let registry_pid = ambitious::whereis(Self::NAME)?;

        match call::<RegistryCall, RegistryReply>(registry_pid, request, Duration::from_secs(5))
            .await
        {
            Ok(reply) => Some(reply),
            Err(e) => {
                tracing::error!(error = ?e, "Registry call failed");
                None
            }
        }
    }
}

#[async_trait]
impl GenServer for Registry {
    type Args = ();

    async fn init(_arg: ()) -> Init<Self> {
        tracing::info!("Room registry started");
        Init::Ok(Registry {
            rooms: HashMap::new(),
        })
    }

    async fn handle_call_raw(&mut self, payload: Vec<u8>, from: From) -> RawReply {
        // Decode the tag to determine message type
        let (tag, msg_payload) = match decode_tag(&payload) {
            Ok((t, p)) => (t, p),
            Err(_) => {
                return RawReply::StopNoReply(ambitious::core::ExitReason::Error(
                    "decode error".into(),
                ));
            }
        };

        match tag {
            "RegistryCall" => {
                if let Ok(msg) = RegistryCall::decode_local(msg_payload) {
                    let result = self.call(msg, from).await;
                    reply_to_raw(result)
                } else {
                    RawReply::StopNoReply(ambitious::core::ExitReason::Error("decode error".into()))
                }
            }
            _ => RawReply::StopNoReply(ambitious::core::ExitReason::Error(format!(
                "unknown call: {}",
                tag
            ))),
        }
    }
}

// Helper to convert Reply<T> to RawReply
fn reply_to_raw<T: Message>(reply: Reply<T>) -> RawReply {
    match reply {
        Reply::Ok(v) => RawReply::Ok(v.encode_local()),
        Reply::Continue(v, arg) => RawReply::Continue(v.encode_local(), arg),
        Reply::Timeout(v, d) => RawReply::Timeout(v.encode_local(), d),
        Reply::NoReply => RawReply::NoReply,
        Reply::Stop(r, v) => RawReply::Stop(r, v.encode_local()),
        Reply::StopNoReply(r) => RawReply::StopNoReply(r),
    }
}

#[async_trait]
impl Call<RegistryCall> for Registry {
    type Reply = RegistryReply;

    async fn call(&mut self, request: RegistryCall, _from: From) -> Reply<RegistryReply> {
        match request {
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

                Reply::Ok(RegistryReply::Room(pid))
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

                Reply::Ok(RegistryReply::Rooms(infos))
            }
        }
    }
}
