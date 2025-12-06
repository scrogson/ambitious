//! Chat room GenServer implementation using v2 API.
//!
//! Each room is a GenServer that stores message history and is registered globally.
//! When users join a room, they fetch history directly from the room.
//!
//! In v2, the struct IS the process state. `init` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::HistoryMessage;
use ambitious::core::{DecodeError, ExitReason};
use ambitious::gen_server::v2::*;
use ambitious::message::{Message, decode_payload, decode_tag, encode_payload, encode_with_tag};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Call requests to Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomCall {
    /// Get message history.
    GetHistory,
}

impl Message for RoomCall {
    fn tag() -> &'static str {
        "RoomCall"
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

/// Call replies from Room.
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

/// Cast messages to Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomCast {
    /// Store a new message.
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

#[async_trait]
impl GenServer for Room {
    type Args = RoomInit;

    async fn init(arg: RoomInit) -> Init<Self> {
        tracing::info!(room = %arg.name, "Room created");
        Init::Ok(Room {
            name: arg.name,
            history: VecDeque::new(),
        })
    }

    async fn handle_call_raw(&mut self, payload: Vec<u8>, from: From) -> RawReply {
        // Decode the tag to determine message type
        let (tag, msg_payload) = match decode_tag(&payload) {
            Ok((t, p)) => (t, p),
            Err(_) => {
                return RawReply::StopNoReply(ExitReason::Error("decode error".into()));
            }
        };

        match tag {
            "RoomCall" => {
                if let Ok(msg) = RoomCall::decode_local(msg_payload) {
                    let result = self.call(msg, from).await;
                    reply_to_raw(result)
                } else {
                    RawReply::StopNoReply(ExitReason::Error("decode error".into()))
                }
            }
            _ => RawReply::StopNoReply(ExitReason::Error(format!("unknown call: {}", tag))),
        }
    }

    async fn handle_cast_raw(&mut self, payload: Vec<u8>) -> Status {
        // Decode the tag to determine message type
        let (tag, msg_payload) = match decode_tag(&payload) {
            Ok((t, p)) => (t, p),
            Err(_) => {
                return Status::Stop(ExitReason::Error("decode error".into()));
            }
        };

        match tag {
            "RoomCast" => {
                if let Ok(msg) = RoomCast::decode_local(msg_payload) {
                    self.cast(msg).await
                } else {
                    Status::Stop(ExitReason::Error("decode error".into()))
                }
            }
            _ => Status::Ok, // Ignore unknown casts
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
impl Call<RoomCall> for Room {
    type Reply = RoomReply;

    async fn call(&mut self, request: RoomCall, _from: From) -> Reply<RoomReply> {
        match request {
            RoomCall::GetHistory => {
                let history: Vec<HistoryMessage> = self.history.iter().cloned().collect();
                Reply::Ok(RoomReply::History(history))
            }
        }
    }
}

#[async_trait]
impl Cast<RoomCast> for Room {
    async fn cast(&mut self, msg: RoomCast) -> Status {
        match msg {
            RoomCast::StoreMessage { from, text } => {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let msg = HistoryMessage {
                    from,
                    text,
                    timestamp,
                };

                self.history.push_back(msg);

                // Trim if too long
                while self.history.len() > MAX_HISTORY {
                    self.history.pop_front();
                }

                Status::Ok
            }
        }
    }
}
