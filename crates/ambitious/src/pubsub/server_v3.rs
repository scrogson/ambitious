//! PubSub GenServer implementation using v3 enum-based dispatch.

use crate::core::{DecodeError, Pid};
use crate::dist::pg;
use crate::gen_server::v3::{
    From, GenServer, Init, Reply, Status, async_trait, call, cast, start,
};
use crate::message::{Message, decode_payload, encode_payload, encode_with_tag};
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

// =============================================================================
// Call Message Enum
// =============================================================================

/// All call (request/response) messages for PubSub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubCall {
    /// Subscribe a PID to a topic.
    Subscribe {
        /// The topic to subscribe to.
        topic: String,
        /// The process to subscribe.
        pid: Pid,
    },
    /// Unsubscribe a PID from a topic.
    Unsubscribe {
        /// The topic to unsubscribe from.
        topic: String,
        /// The process to unsubscribe.
        pid: Pid,
    },
    /// Unsubscribe a PID from all topics.
    UnsubscribeAll {
        /// The process to unsubscribe from all topics.
        pid: Pid,
    },
    /// Get all subscribers for a topic (local + remote).
    GetSubscribers {
        /// The topic to get subscribers for.
        topic: String,
    },
    /// Get local subscribers for a topic.
    GetLocalSubscribers {
        /// The topic to get local subscribers for.
        topic: String,
    },
}

impl Message for PubSubCall {
    fn tag() -> &'static str {
        "PubSubCall"
    }
    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &encode_payload(self))
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
// Cast Message Enum
// =============================================================================

/// All cast (fire-and-forget) messages for PubSub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubCast {
    /// Broadcast a message to all subscribers (local + remote).
    Broadcast {
        /// The topic to broadcast to.
        topic: String,
        /// The message payload.
        payload: Vec<u8>,
        /// Optional PID to exclude from the broadcast.
        exclude: Option<Pid>,
    },
    /// Broadcast a message to local subscribers only.
    LocalBroadcast {
        /// The topic to broadcast to.
        topic: String,
        /// The message payload.
        payload: Vec<u8>,
        /// Optional PID to exclude from the broadcast.
        exclude: Option<Pid>,
    },
    /// Forward a broadcast from another node (internal).
    ForwardBroadcast {
        /// The topic to broadcast to.
        topic: String,
        /// The message payload.
        payload: Vec<u8>,
        /// Optional PID to exclude from the broadcast.
        exclude: Option<Pid>,
    },
}

impl Message for PubSubCast {
    fn tag() -> &'static str {
        "PubSubCast"
    }
    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &encode_payload(self))
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
// Info Message Enum
// =============================================================================

/// Info messages for PubSub (system events, continue messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubInfo {
    /// Complete initialization after we have our PID.
    CompleteInit,
}

impl Message for PubSubInfo {
    fn tag() -> &'static str {
        "PubSubInfo"
    }
    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &encode_payload(self))
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
// Reply Type
// =============================================================================

/// Reply type for PubSub calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PubSubReply {
    /// Empty acknowledgment.
    Ok,
    /// List of subscriber PIDs.
    Subscribers(Vec<Pid>),
}

impl Message for PubSubReply {
    fn tag() -> &'static str {
        "PubSubReply"
    }
    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &encode_payload(self))
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
// PubSub Server (struct IS the state)
// =============================================================================

/// The PubSub GenServer - struct IS the state.
pub struct PubSub {
    /// The name this PubSub is registered under.
    name: String,
    /// Local registry for efficient local dispatch.
    registry: Arc<Registry<String, ()>>,
    /// The pg group name for this PubSub cluster.
    pg_group: String,
    /// Our own PID (for filtering self-broadcasts).
    self_pid: Option<Pid>,
}

#[async_trait]
impl GenServer for PubSub {
    type Args = PubSubConfig;
    type Call = PubSubCall;
    type Cast = PubSubCast;
    type Info = PubSubInfo;
    type Reply = PubSubReply;

    async fn init(config: PubSubConfig) -> Init<Self> {
        let pg_group = format!("pubsub:{}", config.name);
        let registry = Registry::new_duplicate(format!("pubsub:{}", config.name));

        let server = PubSub {
            name: config.name,
            registry,
            pg_group,
            self_pid: None,
        };

        // Complete init after we have our PID via handle_continue
        Init::Continue(server, PubSubInfo::CompleteInit.encode_local())
    }

    async fn handle_call(&mut self, msg: PubSubCall, _from: From) -> Reply<PubSubReply> {
        match msg {
            PubSubCall::Subscribe { topic, pid } => {
                if pid.is_local() {
                    let _ = self.registry.register(topic, pid, ());
                }
                Reply::Ok(PubSubReply::Ok)
            }
            PubSubCall::Unsubscribe { topic, pid } => {
                if pid.is_local() {
                    self.registry.unregister(&topic, pid);
                }
                Reply::Ok(PubSubReply::Ok)
            }
            PubSubCall::UnsubscribeAll { pid } => {
                self.registry.unregister_all(pid);
                Reply::Ok(PubSubReply::Ok)
            }
            PubSubCall::GetSubscribers { topic } => {
                let pg_topic_group = format!("{}:{}", self.pg_group, topic);
                let subscribers = pg::get_members(&pg_topic_group);
                Reply::Ok(PubSubReply::Subscribers(subscribers))
            }
            PubSubCall::GetLocalSubscribers { topic } => {
                let subscribers: Vec<Pid> = self
                    .registry
                    .lookup(&topic)
                    .into_iter()
                    .map(|(pid, _)| pid)
                    .collect();
                Reply::Ok(PubSubReply::Subscribers(subscribers))
            }
        }
    }

    async fn handle_cast(&mut self, msg: PubSubCast) -> Status {
        match msg {
            PubSubCast::Broadcast { topic, payload, exclude } => {
                dispatch_local(&self.registry, &topic, &payload, exclude);
                forward_to_remote_nodes(&self.pg_group, &topic, &payload, exclude, self.self_pid);
                Status::Ok
            }
            PubSubCast::LocalBroadcast { topic, payload, exclude } => {
                dispatch_local(&self.registry, &topic, &payload, exclude);
                Status::Ok
            }
            PubSubCast::ForwardBroadcast { topic, payload, exclude } => {
                dispatch_local(&self.registry, &topic, &payload, exclude);
                Status::Ok
            }
        }
    }

    async fn handle_info(&mut self, msg: PubSubInfo) -> Status {
        match msg {
            PubSubInfo::CompleteInit => {
                let self_pid = crate::current_pid();
                self.self_pid = Some(self_pid);

                pg::join(&self.pg_group, self_pid);
                let _ = crate::register(&self.name, self_pid);

                tracing::debug!(
                    name = %self.name,
                    pg_group = %self.pg_group,
                    pid = ?self_pid,
                    "PubSub started"
                );

                Status::Ok
            }
        }
    }

    async fn handle_continue(&mut self, arg: Vec<u8>) -> Status {
        // Try to decode as PubSubInfo (for continue messages)
        if let Ok((_tag, payload)) = crate::message::decode_tag(&arg) {
            if let Ok(info) = PubSubInfo::decode_local(payload) {
                return self.handle_info(info).await;
            }
        }
        Status::Ok
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
    let members = pg::get_members(pg_group);
    let my_node = crate::core::node::node_name_atom();

    let forward_msg = PubSubCast::ForwardBroadcast {
        topic: topic.to_string(),
        payload: payload.to_vec(),
        exclude,
    };

    // For forwarding to remote nodes, we need to send via the old cast mechanism
    // since they might not be running v3 yet. We'll use raw message sending.
    let msg_bytes = forward_msg.encode_local();
    for pid in members {
        if Some(pid) == self_pid {
            continue;
        }
        if pid.node() != my_node {
            // Send raw - the remote PubSub will handle it via handle_info
            let _ = crate::send_raw(pid, msg_bytes.clone());
        }
    }
}

// =============================================================================
// Public API - Convenience functions for interacting with PubSub
// =============================================================================

impl PubSub {
    /// Start a PubSub server with the given configuration.
    pub async fn start_link(config: PubSubConfig) -> Result<Pid, String> {
        start::<PubSub>(config)
            .await
            .map_err(|e| format!("failed to start PubSub: {:?}", e))
    }

    /// Subscribe the current process to a topic.
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

        call::<PubSub, PubSubReply>(server_pid, request, Duration::from_secs(5))
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

        call::<PubSub, PubSubReply>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("unsubscribe failed: {:?}", e))?;

        Ok(())
    }

    /// Unsubscribe a PID from all topics.
    pub async fn unsubscribe_all(pubsub: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::UnsubscribeAll { pid };

        call::<PubSub, PubSubReply>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("unsubscribe_all failed: {:?}", e))?;

        Ok(())
    }

    /// Broadcast a message to all subscribers of a topic (local + remote).
    pub async fn broadcast<M: Serialize>(
        pubsub: &str,
        topic: &str,
        message: &M,
    ) -> Result<(), String> {
        Self::broadcast_impl(pubsub, topic, message, None).await
    }

    /// Broadcast a message to all subscribers except a specific PID.
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

        cast::<PubSub>(server_pid, cast_msg);
        Ok(())
    }

    /// Broadcast a message to local subscribers only (same node).
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

        cast::<PubSub>(server_pid, cast_msg);
        Ok(())
    }

    /// Get all subscribers for a topic (local + remote).
    pub async fn subscribers(pubsub: &str, topic: &str) -> Result<Vec<Pid>, String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::GetSubscribers {
            topic: topic.to_string(),
        };

        match call::<PubSub, PubSubReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PubSubReply::Subscribers(subs)) => Ok(subs),
            Ok(_) => Err("unexpected reply type".to_string()),
            Err(e) => Err(format!("subscribers failed: {:?}", e)),
        }
    }

    /// Get local subscribers only for a topic.
    pub async fn local_subscribers(pubsub: &str, topic: &str) -> Result<Vec<Pid>, String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = PubSubCall::GetLocalSubscribers {
            topic: topic.to_string(),
        };

        match call::<PubSub, PubSubReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PubSubReply::Subscribers(subs)) => Ok(subs),
            Ok(_) => Err("unexpected reply type".to_string()),
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
