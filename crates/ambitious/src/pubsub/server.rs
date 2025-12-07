//! PubSub GenServer implementation.

use crate::core::{DecodeError, Pid};
use crate::dist::pg;
use crate::gen_server::{
    From, GenServer, HandleCall, HandleCast, HandleInfo, Init, Reply, Status, async_trait, call,
    cast, start,
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
// Message Types
// =============================================================================

/// Subscribe a PID to a topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscribe {
    /// The topic to subscribe to.
    pub topic: String,
    /// The PID to subscribe.
    pub pid: Pid,
}

impl Message for Subscribe {
    fn tag() -> &'static str {
        "Subscribe"
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

/// Unsubscribe a PID from a topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unsubscribe {
    /// The topic to unsubscribe from.
    pub topic: String,
    /// The PID to unsubscribe.
    pub pid: Pid,
}

impl Message for Unsubscribe {
    fn tag() -> &'static str {
        "Unsubscribe"
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

/// Unsubscribe a PID from all topics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeAll {
    /// The PID to unsubscribe.
    pub pid: Pid,
}

impl Message for UnsubscribeAll {
    fn tag() -> &'static str {
        "UnsubscribeAll"
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

/// Get subscribers for a topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetSubscribers {
    /// The topic to get subscribers for.
    pub topic: String,
}

impl Message for GetSubscribers {
    fn tag() -> &'static str {
        "GetSubscribers"
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

/// Get local subscribers for a topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetLocalSubscribers {
    /// The topic to get local subscribers for.
    pub topic: String,
}

impl Message for GetLocalSubscribers {
    fn tag() -> &'static str {
        "GetLocalSubscribers"
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

/// Reply containing a list of subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscribers(pub Vec<Pid>);

impl Message for Subscribers {
    fn tag() -> &'static str {
        "Subscribers"
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

/// Broadcast a message to all subscribers of a topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Broadcast {
    /// The topic to broadcast to.
    pub topic: String,
    /// The serialized message payload.
    pub payload: Vec<u8>,
    /// Optional PID to exclude from broadcast.
    pub exclude: Option<Pid>,
}

impl Message for Broadcast {
    fn tag() -> &'static str {
        "Broadcast"
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

/// Local-only broadcast (don't forward to other nodes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalBroadcast {
    /// The topic to broadcast to.
    pub topic: String,
    /// The serialized message payload.
    pub payload: Vec<u8>,
    /// Optional PID to exclude from broadcast.
    pub exclude: Option<Pid>,
}

impl Message for LocalBroadcast {
    fn tag() -> &'static str {
        "LocalBroadcast"
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

/// Forward a broadcast from another node (internal use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardBroadcast {
    /// The topic to broadcast to.
    pub topic: String,
    /// The serialized message payload.
    pub payload: Vec<u8>,
    /// Optional PID to exclude from broadcast.
    pub exclude: Option<Pid>,
}

impl Message for ForwardBroadcast {
    fn tag() -> &'static str {
        "ForwardBroadcast"
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

/// Internal message to complete initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompleteInit;

impl Message for CompleteInit {
    fn tag() -> &'static str {
        "CompleteInit"
    }
    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::tag(), &[])
    }
    fn decode_local(_bytes: &[u8]) -> Result<Self, DecodeError> {
        Ok(CompleteInit)
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
    /// Key is topic, value is unit (we only care about the PIDs).
    registry: Arc<Registry<String, ()>>,
    /// The pg group name for this PubSub cluster.
    pg_group: String,
    /// Our own PID (for filtering self-broadcasts).
    self_pid: Option<Pid>,
}

// Register handlers for automatic dispatch
crate::register_handlers!(PubSub {
    calls: [
        Subscribe,
        Unsubscribe,
        UnsubscribeAll,
        GetSubscribers,
        GetLocalSubscribers
    ],
    casts: [Broadcast, LocalBroadcast, ForwardBroadcast],
    infos: [CompleteInit],
});

#[async_trait]
impl GenServer for PubSub {
    type Args = PubSubConfig;

    async fn init(config: PubSubConfig) -> Init<Self> {
        let pg_group = format!("pubsub:{}", config.name);

        // Create the local registry for subscriptions
        let registry = Registry::new_duplicate(format!("pubsub:{}", config.name));

        let server = PubSub {
            name: config.name,
            registry,
            pg_group,
            self_pid: None,
        };

        // We'll complete init after we have our PID via handle_info
        Init::Continue(server, CompleteInit.encode_local())
    }
}

#[async_trait]
impl HandleCall<Subscribe> for PubSub {
    type Reply = ();
    type Output = Reply<()>;

    async fn handle_call(&mut self, msg: Subscribe, _from: From) -> Reply<()> {
        // Only add to local registry if PID is local
        if msg.pid.is_local() {
            let _ = self.registry.register(msg.topic, msg.pid, ());
        }
        Reply::Ok(())
    }
}

#[async_trait]
impl HandleCall<Unsubscribe> for PubSub {
    type Reply = ();
    type Output = Reply<()>;

    async fn handle_call(&mut self, msg: Unsubscribe, _from: From) -> Reply<()> {
        if msg.pid.is_local() {
            self.registry.unregister(&msg.topic, msg.pid);
        }
        Reply::Ok(())
    }
}

#[async_trait]
impl HandleCall<UnsubscribeAll> for PubSub {
    type Reply = ();
    type Output = Reply<()>;

    async fn handle_call(&mut self, msg: UnsubscribeAll, _from: From) -> Reply<()> {
        self.registry.unregister_all(msg.pid);
        Reply::Ok(())
    }
}

#[async_trait]
impl HandleCall<GetSubscribers> for PubSub {
    type Reply = Subscribers;
    type Output = Reply<Subscribers>;

    async fn handle_call(&mut self, msg: GetSubscribers, _from: From) -> Reply<Subscribers> {
        // Get all subscribers from pg (includes remote)
        let pg_topic_group = format!("{}:{}", self.pg_group, msg.topic);
        let subscribers = pg::get_members(&pg_topic_group);
        Reply::Ok(Subscribers(subscribers))
    }
}

#[async_trait]
impl HandleCall<GetLocalSubscribers> for PubSub {
    type Reply = Subscribers;
    type Output = Reply<Subscribers>;

    async fn handle_call(&mut self, msg: GetLocalSubscribers, _from: From) -> Reply<Subscribers> {
        let subscribers: Vec<Pid> = self
            .registry
            .lookup(&msg.topic)
            .into_iter()
            .map(|(pid, _)| pid)
            .collect();
        Reply::Ok(Subscribers(subscribers))
    }
}

#[async_trait]
impl HandleCast<Broadcast> for PubSub {
    type Output = Status;

    async fn handle_cast(&mut self, msg: Broadcast) -> Status {
        // Dispatch to local subscribers
        dispatch_local(&self.registry, &msg.topic, &msg.payload, msg.exclude);

        // Forward to other PubSub nodes
        forward_to_remote_nodes(
            &self.pg_group,
            &msg.topic,
            &msg.payload,
            msg.exclude,
            self.self_pid,
        );

        Status::Ok
    }
}

#[async_trait]
impl HandleCast<LocalBroadcast> for PubSub {
    type Output = Status;

    async fn handle_cast(&mut self, msg: LocalBroadcast) -> Status {
        // Only dispatch locally, don't forward
        dispatch_local(&self.registry, &msg.topic, &msg.payload, msg.exclude);
        Status::Ok
    }
}

#[async_trait]
impl HandleCast<ForwardBroadcast> for PubSub {
    type Output = Status;

    async fn handle_cast(&mut self, msg: ForwardBroadcast) -> Status {
        // This came from another node, only dispatch locally
        dispatch_local(&self.registry, &msg.topic, &msg.payload, msg.exclude);
        Status::Ok
    }
}

#[async_trait]
impl HandleInfo<CompleteInit> for PubSub {
    type Output = Status;

    async fn handle_info(&mut self, _msg: CompleteInit) -> Status {
        // Join the pg group for this PubSub
        let self_pid = crate::current_pid();
        self.self_pid = Some(self_pid);

        pg::join(&self.pg_group, self_pid);

        // Register ourselves by name
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
    let forward_msg = ForwardBroadcast {
        topic: topic.to_string(),
        payload: payload.to_vec(),
        exclude,
    };

    let msg_bytes = forward_msg.encode_local();
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

// =============================================================================
// Public API - Convenience functions for interacting with PubSub
// =============================================================================

impl PubSub {
    /// Start a PubSub server with the given configuration.
    ///
    /// The server will be registered under the configured name and join
    /// a pg group for distributed coordination.
    pub async fn start_link(config: PubSubConfig) -> Result<Pid, String> {
        start::<PubSub>(config)
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

        let request = Subscribe {
            topic: topic.to_string(),
            pid,
        };

        call::<Subscribe, ()>(server_pid, request, Duration::from_secs(5))
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

        let request = Unsubscribe {
            topic: topic.to_string(),
            pid,
        };

        call::<Unsubscribe, ()>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("unsubscribe failed: {:?}", e))?;

        Ok(())
    }

    /// Unsubscribe a PID from all topics.
    pub async fn unsubscribe_all(pubsub: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = UnsubscribeAll { pid };

        call::<UnsubscribeAll, ()>(server_pid, request, Duration::from_secs(5))
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

        let cast_msg = Broadcast {
            topic: topic.to_string(),
            payload,
            exclude,
        };

        cast(server_pid, cast_msg);
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

        let cast_msg = LocalBroadcast {
            topic: topic.to_string(),
            payload,
            exclude,
        };

        cast(server_pid, cast_msg);
        Ok(())
    }

    /// Get all subscribers for a topic (local + remote).
    pub async fn subscribers(pubsub: &str, topic: &str) -> Result<Vec<Pid>, String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = GetSubscribers {
            topic: topic.to_string(),
        };

        match call::<GetSubscribers, Subscribers>(server_pid, request, Duration::from_secs(5)).await
        {
            Ok(Subscribers(subs)) => Ok(subs),
            Err(e) => Err(format!("subscribers failed: {:?}", e)),
        }
    }

    /// Get local subscribers only for a topic.
    pub async fn local_subscribers(pubsub: &str, topic: &str) -> Result<Vec<Pid>, String> {
        let server_pid =
            crate::whereis(pubsub).ok_or_else(|| format!("PubSub '{}' not found", pubsub))?;

        let request = GetLocalSubscribers {
            topic: topic.to_string(),
        };

        match call::<GetLocalSubscribers, Subscribers>(server_pid, request, Duration::from_secs(5))
            .await
        {
            Ok(Subscribers(subs)) => Ok(subs),
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
