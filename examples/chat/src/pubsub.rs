//! PubSub implementation using GenServer.
//!
//! A simple publish-subscribe system similar to Phoenix.PubSub.
//! Processes can subscribe to topics and broadcast messages to all subscribers.

use dream::gen_server::{self, prelude::*};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// PubSub GenServer implementation.
pub struct PubSub;

/// PubSub state (internal).
pub struct PubSubState {
    /// Topic -> Set of subscriber PIDs.
    topics: HashMap<String, HashSet<Pid>>,
    /// PID -> Set of subscribed topics (for cleanup on unsubscribe_all).
    subscriptions: HashMap<Pid, HashSet<String>>,
}

/// Call requests to PubSub (internal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubCall {
    /// Get list of subscribers for a topic.
    Subscribers(String),
    /// Get list of all topics.
    Topics,
}

/// Call replies from PubSub (internal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubReply {
    /// List of subscriber PIDs.
    Subscribers(Vec<Pid>),
    /// List of topic names.
    Topics(Vec<String>),
}

/// Cast messages to PubSub (internal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubCast {
    /// Subscribe a PID to a topic.
    Subscribe { topic: String, pid: Pid },
    /// Unsubscribe a PID from a topic.
    Unsubscribe { topic: String, pid: Pid },
    /// Unsubscribe a PID from all topics.
    UnsubscribeAll { pid: Pid },
    /// Broadcast a message to all subscribers of a topic.
    Broadcast {
        topic: String,
        message: Vec<u8>,
        exclude: Option<Pid>,
    },
}

#[async_trait]
impl GenServer for PubSub {
    type State = PubSubState;
    type InitArg = ();
    type Call = PubSubCall;
    type Cast = PubSubCast;
    type Reply = PubSubReply;

    async fn init(_arg: ()) -> InitResult<PubSubState> {
        tracing::info!("PubSub started");
        InitResult::Ok(PubSubState {
            topics: HashMap::new(),
            subscriptions: HashMap::new(),
        })
    }

    async fn handle_call(
        request: PubSubCall,
        _from: From,
        state: &mut PubSubState,
    ) -> CallResult<PubSubState, PubSubReply> {
        match request {
            PubSubCall::Subscribers(topic) => {
                let subscribers: Vec<Pid> = state
                    .topics
                    .get(&topic)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                CallResult::reply(PubSubReply::Subscribers(subscribers), state.clone_state())
            }
            PubSubCall::Topics => {
                let topics: Vec<String> = state.topics.keys().cloned().collect();
                CallResult::reply(PubSubReply::Topics(topics), state.clone_state())
            }
        }
    }

    async fn handle_cast(msg: PubSubCast, state: &mut PubSubState) -> CastResult<PubSubState> {
        match msg {
            PubSubCast::Subscribe { topic, pid } => {
                // Add to topic subscribers
                state.topics.entry(topic.clone()).or_default().insert(pid);
                // Track subscription for this PID
                state.subscriptions.entry(pid).or_default().insert(topic);
                CastResult::noreply(state.clone_state())
            }
            PubSubCast::Unsubscribe { topic, pid } => {
                // Remove from topic subscribers
                if let Some(subscribers) = state.topics.get_mut(&topic) {
                    subscribers.remove(&pid);
                    if subscribers.is_empty() {
                        state.topics.remove(&topic);
                    }
                }
                // Remove from PID's subscriptions
                if let Some(topics) = state.subscriptions.get_mut(&pid) {
                    topics.remove(&topic);
                    if topics.is_empty() {
                        state.subscriptions.remove(&pid);
                    }
                }
                CastResult::noreply(state.clone_state())
            }
            PubSubCast::UnsubscribeAll { pid } => {
                // Get all topics this PID is subscribed to
                if let Some(topics) = state.subscriptions.remove(&pid) {
                    for topic in topics {
                        if let Some(subscribers) = state.topics.get_mut(&topic) {
                            subscribers.remove(&pid);
                            if subscribers.is_empty() {
                                state.topics.remove(&topic);
                            }
                        }
                    }
                }
                CastResult::noreply(state.clone_state())
            }
            PubSubCast::Broadcast {
                topic,
                message,
                exclude,
            } => {
                if let Some(subscribers) = state.topics.get(&topic) {
                    let handle = dream::handle();
                    for &pid in subscribers {
                        if Some(pid) != exclude {
                            let _ = handle.registry().send_raw(pid, message.clone());
                        }
                    }
                }
                CastResult::noreply(state.clone_state())
            }
        }
    }

    async fn handle_info(_msg: Vec<u8>, state: &mut PubSubState) -> InfoResult<PubSubState> {
        InfoResult::noreply(state.clone_state())
    }

    async fn handle_continue(
        _arg: ContinueArg,
        state: &mut PubSubState,
    ) -> ContinueResult<PubSubState> {
        ContinueResult::noreply(state.clone_state())
    }
}

impl PubSubState {
    fn clone_state(&self) -> Self {
        PubSubState {
            topics: self.topics.clone(),
            subscriptions: self.subscriptions.clone(),
        }
    }
}

// =============================================================================
// Client API
// =============================================================================

impl PubSub {
    /// The registered name for the PubSub process.
    pub const NAME: &'static str = "pubsub";

    /// Start the PubSub GenServer.
    pub async fn start() -> Result<Pid, StartError> {
        gen_server::start::<PubSub>(()).await
    }

    /// Subscribe the current process to a topic.
    pub fn subscribe(topic: &str) {
        Self::subscribe_pid(topic, dream::current_pid());
    }

    /// Subscribe a specific PID to a topic.
    pub fn subscribe_pid(topic: &str, pid: Pid) {
        if let Some(pubsub_pid) = dream::whereis(Self::NAME) {
            let _ = gen_server::cast::<PubSub>(
                pubsub_pid,
                PubSubCast::Subscribe {
                    topic: topic.to_string(),
                    pid,
                },
            );
        }
    }

    /// Unsubscribe the current process from a topic.
    pub fn unsubscribe(topic: &str) {
        Self::unsubscribe_pid(topic, dream::current_pid());
    }

    /// Unsubscribe a specific PID from a topic.
    pub fn unsubscribe_pid(topic: &str, pid: Pid) {
        if let Some(pubsub_pid) = dream::whereis(Self::NAME) {
            let _ = gen_server::cast::<PubSub>(
                pubsub_pid,
                PubSubCast::Unsubscribe {
                    topic: topic.to_string(),
                    pid,
                },
            );
        }
    }

    /// Unsubscribe a PID from all topics.
    pub fn unsubscribe_all(pid: Pid) {
        if let Some(pubsub_pid) = dream::whereis(Self::NAME) {
            let _ = gen_server::cast::<PubSub>(pubsub_pid, PubSubCast::UnsubscribeAll { pid });
        }
    }

    /// Broadcast a serializable message to all subscribers of a topic.
    pub fn broadcast<M: Serialize>(topic: &str, message: &M) {
        if let Ok(payload) = postcard::to_allocvec(message) {
            Self::do_broadcast(topic, payload, None);
        }
    }

    /// Broadcast a serializable message to all subscribers except the caller.
    ///
    /// This is useful when the sender is also subscribed to the topic
    /// but shouldn't receive their own message.
    pub fn broadcast_from<M: Serialize>(topic: &str, message: &M) {
        if let Ok(payload) = postcard::to_allocvec(message) {
            Self::do_broadcast(topic, payload, Some(dream::current_pid()));
        }
    }

    /// Internal: send broadcast to PubSub server.
    fn do_broadcast(topic: &str, message: Vec<u8>, exclude: Option<Pid>) {
        if let Some(pubsub_pid) = dream::whereis(Self::NAME) {
            let _ = gen_server::cast::<PubSub>(
                pubsub_pid,
                PubSubCast::Broadcast {
                    topic: topic.to_string(),
                    message,
                    exclude,
                },
            );
        }
    }

    /// Get the list of subscribers for a topic.
    pub async fn subscribers(topic: &str) -> Vec<Pid> {
        let pubsub_pid = match dream::whereis(Self::NAME) {
            Some(pid) => pid,
            None => return vec![],
        };

        match gen_server::call::<PubSub>(
            pubsub_pid,
            PubSubCall::Subscribers(topic.to_string()),
            Duration::from_secs(5),
        )
        .await
        {
            Ok(PubSubReply::Subscribers(subs)) => subs,
            _ => vec![],
        }
    }

    /// Get the list of all topics.
    pub async fn topics() -> Vec<String> {
        let pubsub_pid = match dream::whereis(Self::NAME) {
            Some(pid) => pid,
            None => return vec![],
        };

        match gen_server::call::<PubSub>(pubsub_pid, PubSubCall::Topics, Duration::from_secs(5))
            .await
        {
            Ok(PubSubReply::Topics(topics)) => topics,
            _ => vec![],
        }
    }
}
