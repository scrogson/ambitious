//! Chat room GenServer implementation using v3 enum-based dispatch.
//!
//! Each room is a GenServer that stores message history and is registered globally.
//! When users join a room, they fetch history directly from the room.
//!
//! The struct IS the process state. `init` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::HistoryMessage;
use ambitious::gen_server::v3::{
    async_trait, call, cast, start, From, GenServer, Init, Reply, Status,
};
use ambitious::message::Message;
use ambitious::core::{DecodeError, Pid};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum number of messages to keep in history.
const MAX_HISTORY: usize = 50;

/// Room GenServer - the struct IS the state.
pub struct Room {
    /// Room name.
    #[allow(dead_code)]
    name: String,
    /// Message history.
    history: VecDeque<HistoryMessage>,
}

/// Initialization argument for Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInit {
    /// Room name.
    pub name: String,
}

// =============================================================================
// Call Message Enum
// =============================================================================

/// All call (request/response) messages for Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomCall {
    /// Request to get message history.
    GetHistory,
}

impl Message for RoomCall {
    fn tag() -> &'static str {
        "RoomCall"
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
// Cast Message Enum
// =============================================================================

/// All cast (fire-and-forget) messages for Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomCast {
    /// Store a new message in the room history.
    StoreMessage {
        /// Who sent the message.
        from: String,
        /// Message text.
        text: String,
    },
}

impl Message for RoomCast {
    fn tag() -> &'static str {
        "RoomCast"
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

/// Reply type for Room calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomReply {
    /// Message history.
    History(Vec<HistoryMessage>),
}

impl Message for RoomReply {
    fn tag() -> &'static str {
        "RoomReply"
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
// GenServer Implementation
// =============================================================================

#[async_trait]
impl GenServer for Room {
    type Args = RoomInit;
    type Call = RoomCall;
    type Cast = RoomCast;
    type Info = ();
    type Reply = RoomReply;

    async fn init(arg: RoomInit) -> Init<Self> {
        tracing::info!(room = %arg.name, "Room created");
        Init::Ok(Room {
            name: arg.name,
            history: VecDeque::new(),
        })
    }

    async fn handle_call(&mut self, msg: RoomCall, _from: From) -> Reply<RoomReply> {
        match msg {
            RoomCall::GetHistory => {
                let history: Vec<HistoryMessage> = self.history.iter().cloned().collect();
                Reply::Ok(RoomReply::History(history))
            }
        }
    }

    async fn handle_cast(&mut self, msg: RoomCast) -> Status {
        match msg {
            RoomCast::StoreMessage { from, text } => {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let history_msg = HistoryMessage {
                    from,
                    text,
                    timestamp,
                };

                self.history.push_back(history_msg);

                // Trim if too long
                while self.history.len() > MAX_HISTORY {
                    self.history.pop_front();
                }

                Status::Ok
            }
        }
    }

    async fn handle_info(&mut self, _msg: ()) -> Status {
        Status::Ok
    }
}

// =============================================================================
// Public API (unused in favor of direct v3 API, but kept for reference)
// =============================================================================

#[allow(dead_code)]
impl Room {
    /// Start a new Room GenServer.
    pub async fn start_room(name: String) -> Result<Pid, ambitious::gen_server::v3::Error> {
        start::<Room>(RoomInit { name }).await
    }

    /// Get message history from a room.
    pub async fn get_history(room_pid: Pid) -> Result<Vec<HistoryMessage>, String> {
        match call::<Room, RoomReply>(room_pid, RoomCall::GetHistory, Duration::from_secs(5)).await
        {
            Ok(RoomReply::History(history)) => Ok(history),
            Err(e) => Err(format!("get_history failed: {:?}", e)),
        }
    }

    /// Store a message in the room history.
    pub fn store_message(room_pid: Pid, from: String, text: String) {
        cast::<Room>(room_pid, RoomCast::StoreMessage { from, text });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_init() {
        let init = RoomInit {
            name: "test".to_string(),
        };
        assert_eq!(init.name, "test");
    }
}
