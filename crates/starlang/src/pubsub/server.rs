//! PubSub GenServer implementation.

use crate::core::{Pid, RawTerm};
use crate::dist::pg;
use crate::gen_server::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, GenServer, InfoResult, InitResult,
    async_trait,
};
use crate::registry::Registry;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for starting a PubSub server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubSubConfig {
    /// The name to register the PubSub process under.
    pub name: String,
}

impl PubSubConfig {
    /// Create a new PubSub configuration with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// Messages that can be sent to the PubSub server via call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubCall {
    /// Subscribe a PID to a topic.
    Subscribe { topic: String, pid: Pid },
    /// Unsubscribe a PID from a topic.
    Unsubscribe { topic: String, pid: Pid },
    /// Unsubscribe a PID from all topics.
    UnsubscribeAll { pid: Pid },
    /// Get subscribers for a topic.
    Subscribers { topic: String },
    /// Get local subscribers for a topic.
    LocalSubscribers { topic: String },
}

/// Messages that can be sent to the PubSub server via cast (fire-and-forget).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubCast {
    /// Broadcast a message to all subscribers of a topic.
    Broadcast {
        topic: String,
        payload: Vec<u8>,
        exclude: Option<Pid>,
    },
    /// Local-only broadcast (don't forward to other nodes).
    LocalBroadcast {
        topic: String,
        payload: Vec<u8>,
        exclude: Option<Pid>,
    },
    /// Forward a broadcast from another node (internal use).
    ForwardBroadcast {
        topic: String,
        payload: Vec<u8>,
        exclude: Option<Pid>,
    },
}

/// Reply from PubSub calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubReply {
    /// Operation succeeded.
    Ok,
    /// List of subscribers.
    Subscribers(Vec<Pid>),
}

/// Internal state of the PubSub server.
pub struct PubSubState {
    /// The name this PubSub is registered under.
    name: String,
    /// Local registry for efficient local dispatch.
    /// Key is topic, value is unit (we only care about the PIDs).
    registry: Arc<Registry<String, ()>>,
    /// The pg group name for this PubSub cluster.
    pg_group: String,
    /// Our own PID (for filtering self-broadcasts).
    self_pid: Option<Pid>,
}

/// The PubSub GenServer.
pub struct PubSub;

#[async_trait]
impl GenServer for PubSub {
    type State = PubSubState;
    type InitArg = PubSubConfig;
    type Call = PubSubCall;
    type Cast = PubSubCast;
    type Reply = PubSubReply;

    async fn init(config: PubSubConfig) -> InitResult<PubSubState> {
        let pg_group = format!("pubsub:{}", config.name);

        // Create the local registry for subscriptions
        let registry = Registry::new_duplicate(format!("pubsub:{}", config.name));

        let state = PubSubState {
            name: config.name,
            registry,
            pg_group,
            self_pid: None,
        };

        // We'll join the pg group in handle_continue after we have our PID
        InitResult::ok_continue(state, &())
    }

    async fn handle_call(
        request: PubSubCall,
        _from: From,
        state: &mut PubSubState,
    ) -> CallResult<PubSubState, PubSubReply> {
        match request {
            PubSubCall::Subscribe { topic, pid } => {
                // Only add to local registry if PID is local
                if pid.is_local() {
                    let _ = state.registry.register(topic, pid, ());
                }
                CallResult::Reply(PubSubReply::Ok, std::mem::take(state))
            }
            PubSubCall::Unsubscribe { topic, pid } => {
                if pid.is_local() {
                    state.registry.unregister(&topic, pid);
                }
                CallResult::Reply(PubSubReply::Ok, std::mem::take(state))
            }
            PubSubCall::UnsubscribeAll { pid } => {
                state.registry.unregister_all(pid);
                CallResult::Reply(PubSubReply::Ok, std::mem::take(state))
            }
            PubSubCall::Subscribers { topic } => {
                // Get all subscribers from pg (includes remote)
                let pg_topic_group = format!("{}:{}", state.pg_group, topic);
                let subscribers = pg::get_members(&pg_topic_group);
                CallResult::Reply(PubSubReply::Subscribers(subscribers), std::mem::take(state))
            }
            PubSubCall::LocalSubscribers { topic } => {
                let subscribers: Vec<Pid> = state
                    .registry
                    .lookup(&topic)
                    .into_iter()
                    .map(|(pid, _)| pid)
                    .collect();
                CallResult::Reply(PubSubReply::Subscribers(subscribers), std::mem::take(state))
            }
        }
    }

    async fn handle_cast(msg: PubSubCast, state: &mut PubSubState) -> CastResult<PubSubState> {
        match msg {
            PubSubCast::Broadcast {
                topic,
                payload,
                exclude,
            } => {
                // Dispatch to local subscribers
                dispatch_local(&state.registry, &topic, &payload, exclude);

                // Forward to other PubSub nodes
                forward_to_remote_nodes(&state.pg_group, &topic, &payload, exclude, state.self_pid);

                CastResult::NoReply(std::mem::take(state))
            }
            PubSubCast::LocalBroadcast {
                topic,
                payload,
                exclude,
            } => {
                // Only dispatch locally, don't forward
                dispatch_local(&state.registry, &topic, &payload, exclude);
                CastResult::NoReply(std::mem::take(state))
            }
            PubSubCast::ForwardBroadcast {
                topic,
                payload,
                exclude,
            } => {
                // This came from another node, only dispatch locally
                dispatch_local(&state.registry, &topic, &payload, exclude);
                CastResult::NoReply(std::mem::take(state))
            }
        }
    }

    async fn handle_info(_msg: RawTerm, state: &mut PubSubState) -> InfoResult<PubSubState> {
        // Handle any raw messages (e.g., from pg or other processes)
        InfoResult::NoReply(std::mem::take(state))
    }

    async fn handle_continue(
        _arg: ContinueArg,
        state: &mut PubSubState,
    ) -> ContinueResult<PubSubState> {
        // Join the pg group for this PubSub
        let self_pid = crate::current_pid();
        state.self_pid = Some(self_pid);

        pg::join(&state.pg_group, self_pid);

        // Register ourselves by name
        let _ = crate::register(&state.name, self_pid);

        tracing::debug!(
            name = %state.name,
            pg_group = %state.pg_group,
            pid = ?self_pid,
            "PubSub started"
        );

        ContinueResult::NoReply(std::mem::take(state))
    }
}

impl Default for PubSubState {
    fn default() -> Self {
        Self {
            name: String::new(),
            registry: Registry::new_duplicate("default"),
            pg_group: String::new(),
            self_pid: None,
        }
    }
}

/// Dispatch a message to local subscribers.
fn dispatch_local(
    registry: &Registry<String, ()>,
    topic: &str,
    payload: &[u8],
    exclude: Option<Pid>,
) {
    registry.dispatch(&topic.to_string(), |entries| {
        for (pid, _) in entries {
            if Some(*pid) != exclude {
                let _ = crate::send_raw(*pid, payload.to_vec());
            }
        }
    });
}

/// Forward a broadcast to PubSub processes on other nodes.
fn forward_to_remote_nodes(
    pg_group: &str,
    topic: &str,
    payload: &[u8],
    exclude: Option<Pid>,
    self_pid: Option<Pid>,
) {
    // Get all PubSub processes in our cluster
    let members = pg::get_members(pg_group);
    let my_node = crate::core::node::node_name_atom();

    // Send ForwardBroadcast to each remote PubSub
    let forward_msg = PubSubCast::ForwardBroadcast {
        topic: topic.to_string(),
        payload: payload.to_vec(),
        exclude,
    };

    if let Ok(msg_bytes) = postcard::to_allocvec(&forward_msg) {
        for pid in members {
            // Skip ourselves
            if Some(pid) == self_pid {
                continue;
            }
            // Only send to remote nodes
            if pid.node() != my_node {
                let _ = crate::send_raw(pid, msg_bytes.clone());
            }
        }
    }
}

// =============================================================================
// Public API - Convenience functions for interacting with PubSub
// =============================================================================

impl PubSub {
    /// Start a PubSub server with the given configuration.
    ///
    /// The server will be registered under the configured name and join
    /// a pg group for distributed coordination.
    pub async fn start_link(config: PubSubConfig) -> Result<Pid, String> {
        crate::gen_server::start::<PubSub>(config)
            .await
            .map_err(|e| format!("failed to start PubSub: {:?}", e))
    }

    /// Subscribe the current process to a topic.
    ///
    /// # Arguments
    ///
    /// * `pubsub` - The name of the PubSub server
    /// * `topic` - The topic to subscribe to
    pub async fn subscribe(pubsub: &str, topic: &str) -> Result<(), String> {
        Self::subscribe_pid(pubsub, topic, crate::current_pid()).await
    }

    /// Subscribe a specific PID to a topic.
    pub async fn subscribe_pid(pubsub: &str, topic: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::Subscribe {
            topic: topic.to_string(),
            pid,
        };

        crate::gen_server::call::<PubSub>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("subscribe failed: {:?}", e))?;

        Ok(())
    }

    /// Unsubscribe the current process from a topic.
    pub async fn unsubscribe(pubsub: &str, topic: &str) -> Result<(), String> {
        Self::unsubscribe_pid(pubsub, topic, crate::current_pid()).await
    }

    /// Unsubscribe a specific PID from a topic.
    pub async fn unsubscribe_pid(pubsub: &str, topic: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::Unsubscribe {
            topic: topic.to_string(),
            pid,
        };

        crate::gen_server::call::<PubSub>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("unsubscribe failed: {:?}", e))?;

        Ok(())
    }

    /// Unsubscribe a PID from all topics.
    pub async fn unsubscribe_all(pubsub: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::UnsubscribeAll { pid };

        crate::gen_server::call::<PubSub>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("unsubscribe_all failed: {:?}", e))?;

        Ok(())
    }

    /// Broadcast a message to all subscribers of a topic (local + remote).
    ///
    /// The message is sent to all local subscribers directly, then forwarded
    /// to PubSub processes on other nodes which dispatch to their local subscribers.
    pub async fn broadcast<M: Serialize>(
        pubsub: &str,
        topic: &str,
        message: &M,
    ) -> Result<(), String> {
        Self::broadcast_impl(pubsub, topic, message, None).await
    }

    /// Broadcast a message to all subscribers except a specific PID.
    ///
    /// This is useful when the sender is also subscribed to the topic
    /// but shouldn't receive their own message.
    pub async fn broadcast_from<M: Serialize>(
        pubsub: &str,
        topic: &str,
        exclude: Pid,
        message: &M,
    ) -> Result<(), String> {
        Self::broadcast_impl(pubsub, topic, message, Some(exclude)).await
    }

    async fn broadcast_impl<M: Serialize>(
        pubsub: &str,
        topic: &str,
        message: &M,
        exclude: Option<Pid>,
    ) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let payload =
            postcard::to_allocvec(message).map_err(|e| format!("serialize failed: {}", e))?;

        let cast_msg = PubSubCast::Broadcast {
            topic: topic.to_string(),
            payload,
            exclude,
        };

        let _ = crate::gen_server::cast::<PubSub>(server_pid, cast_msg);
        Ok(())
    }

    /// Broadcast a message to local subscribers only (same node).
    ///
    /// This does not forward the message to other nodes.
    pub async fn local_broadcast<M: Serialize>(
        pubsub: &str,
        topic: &str,
        message: &M,
    ) -> Result<(), String> {
        Self::local_broadcast_impl(pubsub, topic, message, None).await
    }

    /// Broadcast to local subscribers, excluding a specific PID.
    pub async fn local_broadcast_from<M: Serialize>(
        pubsub: &str,
        topic: &str,
        exclude: Pid,
        message: &M,
    ) -> Result<(), String> {
        Self::local_broadcast_impl(pubsub, topic, message, Some(exclude)).await
    }

    async fn local_broadcast_impl<M: Serialize>(
        pubsub: &str,
        topic: &str,
        message: &M,
        exclude: Option<Pid>,
    ) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let payload =
            postcard::to_allocvec(message).map_err(|e| format!("serialize failed: {}", e))?;

        let cast_msg = PubSubCast::LocalBroadcast {
            topic: topic.to_string(),
            payload,
            exclude,
        };

        let _ = crate::gen_server::cast::<PubSub>(server_pid, cast_msg);
        Ok(())
    }

    /// Get all subscribers for a topic (local + remote).
    pub async fn subscribers(pubsub: &str, topic: &str) -> Result<Vec<Pid>, String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::Subscribers {
            topic: topic.to_string(),
        };

        match crate::gen_server::call::<PubSub>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PubSubReply::Subscribers(subs)) => Ok(subs),
            Ok(_) => Err("unexpected reply".to_string()),
            Err(e) => Err(format!("subscribers failed: {:?}", e)),
        }
    }

    /// Get local subscribers only for a topic.
    pub async fn local_subscribers(pubsub: &str, topic: &str) -> Result<Vec<Pid>, String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::LocalSubscribers {
            topic: topic.to_string(),
        };

        match crate::gen_server::call::<PubSub>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PubSubReply::Subscribers(subs)) => Ok(subs),
            Ok(_) => Err("unexpected reply".to_string()),
            Err(e) => Err(format!("local_subscribers failed: {:?}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pubsub_config() {
        let config = PubSubConfig::new("test");
        assert_eq!(config.name, "test");
    }
}
