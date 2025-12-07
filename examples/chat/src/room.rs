//! Chat room GenServer implementation using v2 API.
//!
//! Each room is a GenServer that stores message history and is registered globally.
//! When users join a room, they fetch history directly from the room.
//!
//! In v2, the struct IS the process state. `init` constructs it,
//! and handlers mutate it via `&mut self`.

use crate::protocol::HistoryMessage;
use ambitious::{Message, call, cast};
use ambitious::gen_server::v2::*;
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
/// Note: This still needs Serialize/Deserialize since it's not a Message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInit {
    /// Room name.
    pub name: String,
}

// =============================================================================
// Call Messages
// =============================================================================

/// Request to get message history.
#[derive(Debug, Clone, Message)]
pub struct GetHistory;

/// Response containing message history.
#[derive(Debug, Clone, Message)]
pub struct History(pub Vec<HistoryMessage>);

// =============================================================================
// Cast Messages
// =============================================================================

/// Store a new message in the room history.
#[derive(Debug, Clone, Message)]
pub struct StoreMessage {
    /// Who sent the message.
    pub from: String,
    /// Message text.
    pub text: String,
}

// =============================================================================
// GenServer Implementation
// =============================================================================

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
}

// =============================================================================
// Call Handlers
// =============================================================================

#[call]
#[async_trait]
impl Call<GetHistory> for Room {
    type Reply = History;
    type Output = Reply<History>;

    async fn call(&mut self, _request: GetHistory, _from: From) -> Reply<History> {
        let history: Vec<HistoryMessage> = self.history.iter().cloned().collect();
        Reply::Ok(History(history))
    }
}

// =============================================================================
// Cast Handlers
// =============================================================================

#[cast]
#[async_trait]
impl Cast<StoreMessage> for Room {
    type Output = Status;

    async fn cast(&mut self, msg: StoreMessage) -> Status {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let history_msg = HistoryMessage {
            from: msg.from,
            text: msg.text,
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
