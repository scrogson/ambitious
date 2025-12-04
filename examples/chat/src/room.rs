//! Chat room GenServer implementation.
//!
//! Each room is a GenServer that stores message history and is registered globally.
//! When users join a room, they fetch history directly from the room.

use crate::protocol::HistoryMessage;
use serde::{Deserialize, Serialize};
use starlang::RawTerm;
use starlang::gen_server::prelude::*;
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of messages to keep in history.
const MAX_HISTORY: usize = 50;

/// Room GenServer implementation.
pub struct Room;

/// Room state.
pub struct RoomState {
    /// Room name.
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

/// Call replies from Room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomReply {
    /// Message history.
    History(Vec<HistoryMessage>),
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

#[async_trait]
impl GenServer for Room {
    type State = RoomState;
    type InitArg = RoomInit;
    type Call = RoomCall;
    type Cast = RoomCast;
    type Reply = RoomReply;

    async fn init(arg: RoomInit) -> InitResult<RoomState> {
        tracing::info!(room = %arg.name, "Room created");
        InitResult::Ok(RoomState {
            name: arg.name,
            history: VecDeque::new(),
        })
    }

    async fn handle_call(
        request: RoomCall,
        _from: From,
        state: &mut RoomState,
    ) -> CallResult<RoomState, RoomReply> {
        match request {
            RoomCall::GetHistory => {
                let history: Vec<HistoryMessage> = state.history.iter().cloned().collect();
                CallResult::reply(
                    RoomReply::History(history),
                    RoomState {
                        name: state.name.clone(),
                        history: state.history.clone(),
                    },
                )
            }
        }
    }

    async fn handle_cast(msg: RoomCast, state: &mut RoomState) -> CastResult<RoomState> {
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

                state.history.push_back(msg);

                // Trim if too long
                while state.history.len() > MAX_HISTORY {
                    state.history.pop_front();
                }

                CastResult::noreply(RoomState {
                    name: state.name.clone(),
                    history: state.history.clone(),
                })
            }
        }
    }

    async fn handle_info(_msg: RawTerm, state: &mut RoomState) -> InfoResult<RoomState> {
        CastResult::noreply(RoomState {
            name: state.name.clone(),
            history: state.history.clone(),
        })
    }

    async fn handle_continue(
        _arg: ContinueArg,
        state: &mut RoomState,
    ) -> ContinueResult<RoomState> {
        CastResult::noreply(RoomState {
            name: state.name.clone(),
            history: state.history.clone(),
        })
    }
}
