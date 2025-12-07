//! Presence GenServer implementation.

use super::crdt::{PresenceCrdt, PresenceDelta as CrdtDelta, Tag, TrackedEntry};
use super::types::{PresenceDiff, PresenceMessage, PresenceMeta, PresenceRef, PresenceState};
use crate::core::{Atom, DecodeError, Pid, RawTerm};
use crate::dist::pg;
use crate::distribution::{NodeDown, monitor_node};
use crate::gen_server::{
    From, GenServer, HandleCall, HandleCast, HandleInfo, Init, Reply, Status, async_trait, call,
    start,
};
use crate::message::{Message, decode_payload, encode_payload, encode_with_tag};
use crate::pubsub::PubSub;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Configuration for starting a Presence server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceConfig {
    /// The name to register the Presence process under.
    pub name: String,
    /// The name of the PubSub server to use for broadcasting.
    pub pubsub: String,
}

impl PresenceConfig {
    /// Create a new Presence configuration.
    pub fn new(name: impl Into<String>, pubsub: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pubsub: pubsub.into(),
        }
    }
}

// =============================================================================
// Message Types
// =============================================================================

/// Track a presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    /// The topic to track presence on.
    pub topic: String,
    /// The key (usually user ID) to track.
    pub key: String,
    /// The PID being tracked.
    pub pid: Pid,
    /// Serialized metadata.
    pub meta: Vec<u8>,
}

impl Message for Track {
    fn tag() -> &'static str {
        "Track"
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

/// Untrack a presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Untrack {
    /// The topic.
    pub topic: String,
    /// The key.
    pub key: String,
    /// The PID to untrack.
    pub pid: Pid,
}

impl Message for Untrack {
    fn tag() -> &'static str {
        "Untrack"
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

/// Untrack all presences for a PID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrackAll {
    /// The PID to untrack.
    pub pid: Pid,
}

impl Message for UntrackAll {
    fn tag() -> &'static str {
        "UntrackAll"
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

/// Update presence metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Update {
    /// The topic.
    pub topic: String,
    /// The key.
    pub key: String,
    /// The PID.
    pub pid: Pid,
    /// New serialized metadata.
    pub meta: Vec<u8>,
}

impl Message for Update {
    fn tag() -> &'static str {
        "Update"
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

/// List presences for a topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {
    /// The topic to list.
    pub topic: String,
}

impl Message for List {
    fn tag() -> &'static str {
        "List"
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

/// Get presence for a specific key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Get {
    /// The topic.
    pub topic: String,
    /// The key.
    pub key: String,
}

impl Message for Get {
    fn tag() -> &'static str {
        "Get"
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

/// Reply containing presence list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceListReply(pub HashMap<String, PresenceState>);

impl Message for PresenceListReply {
    fn tag() -> &'static str {
        "PresenceListReply"
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

/// Reply containing single presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceGetReply(pub Option<PresenceState>);

impl Message for PresenceGetReply {
    fn tag() -> &'static str {
        "PresenceGetReply"
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

/// Apply a delta from another node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyDelta {
    /// The topic.
    pub topic: String,
    /// The presence diff.
    pub diff: PresenceDiff,
}

impl Message for ApplyDelta {
    fn tag() -> &'static str {
        "ApplyDelta"
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

/// Sync full state from another node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    /// The topic.
    pub topic: String,
    /// The full state.
    pub state: HashMap<String, PresenceState>,
}

impl Message for SyncState {
    fn tag() -> &'static str {
        "SyncState"
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
        "PresenceCompleteInit"
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
// Presence Server (struct IS the state)
// =============================================================================

/// The Presence GenServer - struct IS the state.
pub struct Presence {
    /// The name this Presence is registered under.
    name: String,
    /// The PubSub server name.
    pubsub: String,
    /// The pg group for presence servers.
    pg_group: String,
    /// CRDT-based presence state.
    crdt: PresenceCrdt,
    /// Our own PID.
    self_pid: Option<Pid>,
    /// Nodes we're monitoring for disconnect.
    monitored_nodes: HashSet<Atom>,
}

// Register handlers for automatic dispatch
crate::register_handlers!(Presence {
    calls: [Track, Untrack, UntrackAll, Update, List, Get],
    casts: [ApplyDelta, SyncState],
    infos: [CompleteInit],
});

#[async_trait]
impl GenServer for Presence {
    type Args = PresenceConfig;

    async fn init(config: PresenceConfig) -> Init<Self> {
        let pg_group = format!("presence:{}", config.name);

        // Get node name for replica identifier
        let replica = crate::core::node::node_name_atom().as_str();

        let server = Presence {
            name: config.name,
            pubsub: config.pubsub,
            pg_group,
            crdt: PresenceCrdt::new(replica),
            self_pid: None,
            monitored_nodes: HashSet::new(),
        };

        // Register and subscribe in handle_continue
        Init::Continue(server, CompleteInit.encode_local())
    }

    // Override handle_info_raw to handle raw messages (NodeDown, CRDT, legacy)
    async fn handle_info_raw(&mut self, payload: Vec<u8>) -> Status {
        // Check for NodeDown message (node disconnect notification)
        if let Ok(node_down) = postcard::from_bytes::<NodeDown>(&payload) {
            return self.handle_node_down(node_down).await;
        }

        // Try to decode as CRDT delta first (new format)
        if let Ok(crdt_msg) = postcard::from_bytes::<CrdtPresenceMessage>(&payload) {
            return self.handle_crdt_message(crdt_msg).await;
        }

        // Fall back to legacy format for backwards compatibility
        if let Ok(presence_msg) = postcard::from_bytes::<PresenceMessage>(&payload) {
            return self.handle_legacy_message(presence_msg).await;
        }

        // Try CompleteInit
        let raw = RawTerm::from(payload);
        if let Some(_complete_init) = raw.decode::<CompleteInit>() {
            return self.complete_init().await;
        }

        Status::Ok
    }
}

impl Presence {
    async fn handle_node_down(&mut self, node_down: NodeDown) -> Status {
        tracing::info!(
            node = %node_down.node_name,
            reason = ?node_down.reason,
            "Presence detected node disconnect, removing presences"
        );

        // Remove all presences from the disconnected node
        let removed = self.crdt.remove_down_replica(&node_down.node_name);
        self.monitored_nodes
            .remove(&Atom::new(&node_down.node_name));

        if !removed.is_empty() {
            tracing::debug!(
                removed_count = removed.len(),
                "Removed presences from disconnected node"
            );

            // Broadcast leaves for each topic
            let mut by_topic: HashMap<String, Vec<TrackedEntry>> = HashMap::new();
            for entry in removed {
                by_topic.entry(entry.topic.clone()).or_default().push(entry);
            }

            for (topic, entries) in by_topic {
                let mut leaves: HashMap<String, PresenceState> = HashMap::new();
                for entry in entries {
                    leaves
                        .entry(entry.key.clone())
                        .or_insert_with(|| PresenceState { metas: vec![] })
                        .metas
                        .push(entry_to_meta(&entry));
                }

                let diff = PresenceDiff {
                    joins: HashMap::new(),
                    leaves,
                };
                broadcast_delta(&self.pubsub, &self.name, &topic, diff).await;
            }
        }

        Status::Ok
    }

    async fn handle_crdt_message(&mut self, msg: CrdtPresenceMessage) -> Status {
        match msg {
            CrdtPresenceMessage::Delta { topic, delta } => {
                tracing::debug!(
                    topic = %topic,
                    joins = delta.joins.len(),
                    leaves = delta.leave_tags.len(),
                    "Presence received CRDT delta from remote node"
                );

                // Check if any of the joins are from a node we're not monitoring
                let my_node = crate::core::node::node_name_atom();
                for entry in &delta.joins {
                    let replica = Atom::new(&entry.tag.replica);
                    if replica != my_node
                        && !self.monitored_nodes.contains(&replica)
                        && let Ok(_monitor_ref) = monitor_node(replica)
                    {
                        self.monitored_nodes.insert(replica);
                        tracing::info!(
                            node = %entry.tag.replica,
                            my_pid = ?crate::runtime::current_pid(),
                            "Presence monitoring remote node for disconnect (from delta)"
                        );

                        // Send our state to them since they're new
                        let our_delta = self.crdt.extract_all();
                        if !our_delta.is_empty() {
                            let response = CrdtPresenceMessage::Delta {
                                topic: topic.clone(),
                                delta: our_delta,
                            };
                            if let Ok(msg_bytes) = postcard::to_allocvec(&response) {
                                // Find their presence server PID
                                for peer_pid in pg::get_members(&self.pg_group) {
                                    if peer_pid.node() == replica {
                                        tracing::debug!(
                                            peer = ?peer_pid,
                                            "Sending our presence state to new remote node"
                                        );
                                        let _ = crate::send_raw(peer_pid, msg_bytes.clone());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }

                let _result = self.crdt.merge(&delta);
            }
            CrdtPresenceMessage::SyncRequest { from, context } => {
                tracing::debug!(
                    from = ?from,
                    context_size = context.len(),
                    "Presence received CRDT sync request"
                );

                // Monitor the requesting node for disconnects
                let from_node = from.node();
                let my_node = crate::core::node::node_name_atom();
                if from_node != my_node
                    && !self.monitored_nodes.contains(&from_node)
                    && let Ok(_monitor_ref) = monitor_node(from_node)
                {
                    self.monitored_nodes.insert(from_node);
                    tracing::debug!(
                        node = %from_node,
                        "Presence monitoring remote node for disconnect (from sync request)"
                    );
                }

                // Extract only what they need
                let delta = self.crdt.extract(&context);

                if !delta.is_empty() {
                    let response = CrdtPresenceMessage::SyncResponse { delta };
                    if let Ok(msg_bytes) = postcard::to_allocvec(&response) {
                        let _ = crate::send_raw(from, msg_bytes);
                    }
                }
            }
            CrdtPresenceMessage::SyncResponse { delta } => {
                tracing::debug!(
                    joins = delta.joins.len(),
                    "Presence received CRDT sync response"
                );
                let _result = self.crdt.merge(&delta);
            }
        }
        Status::Ok
    }

    async fn handle_legacy_message(&mut self, msg: PresenceMessage) -> Status {
        match msg {
            PresenceMessage::Delta { topic, diff } => {
                tracing::trace!(
                    topic = %topic,
                    joins = diff.joins.len(),
                    leaves = diff.leaves.len(),
                    "Presence received legacy delta from remote node"
                );
                let crdt_delta = presence_diff_to_crdt_delta(&topic, &diff);
                let _result = self.crdt.merge(&crdt_delta);
            }
            PresenceMessage::StateSync {
                topic,
                state: remote_state,
            } => {
                tracing::trace!(
                    topic = %topic,
                    remote_keys = remote_state.len(),
                    "Presence received legacy state sync"
                );
                let crdt_delta = presence_state_to_crdt_delta(&topic, &remote_state);
                let _result = self.crdt.merge(&crdt_delta);
            }
            PresenceMessage::SyncRequest { from } => {
                tracing::debug!(
                    from = ?from,
                    "Presence received legacy sync request, sending CRDT state"
                );

                // Send full state as CRDT delta
                let delta = self.crdt.extract_all();

                if !delta.is_empty() {
                    let response = CrdtPresenceMessage::SyncResponse { delta };
                    if let Ok(msg_bytes) = postcard::to_allocvec(&response) {
                        let _ = crate::send_raw(from, msg_bytes);
                    }
                }
            }
            PresenceMessage::SyncResponse {
                state: remote_state,
            } => {
                tracing::debug!(
                    topics = remote_state.len(),
                    "Presence received legacy sync response"
                );
                for (topic, topic_state) in remote_state {
                    let crdt_delta = presence_state_to_crdt_delta(&topic, &topic_state);
                    let _result = self.crdt.merge(&crdt_delta);
                }
            }
        }
        Status::Ok
    }

    async fn complete_init(&mut self) -> Status {
        let self_pid = crate::current_pid();
        self.self_pid = Some(self_pid);

        // Register ourselves by name
        let _ = crate::register(&self.name, self_pid);

        // Get existing members BEFORE joining the group
        let existing_members = pg::get_members(&self.pg_group);

        // Join the pg group so other Presence servers can find us
        pg::join(&self.pg_group, self_pid);

        tracing::debug!(
            name = %self.name,
            pubsub = %self.pubsub,
            pg_group = %self.pg_group,
            pid = ?self_pid,
            replica = %self.crdt.replica(),
            "Presence started with CRDT"
        );

        // Request state from existing Presence servers on other nodes
        let my_node = crate::core::node::node_name_atom();

        // Send CRDT sync request with our context and monitor remote nodes
        let sync_request = CrdtPresenceMessage::SyncRequest {
            from: self_pid,
            context: self.crdt.context().clone(),
        };
        if let Ok(msg_bytes) = postcard::to_allocvec(&sync_request) {
            for peer_pid in existing_members {
                // Only request from remote nodes (not ourselves)
                if peer_pid != self_pid && peer_pid.node() != my_node {
                    let peer_node = peer_pid.node();

                    // Monitor this node for disconnects (if not already monitoring)
                    if !self.monitored_nodes.contains(&peer_node)
                        && let Ok(_monitor_ref) = monitor_node(peer_node)
                    {
                        self.monitored_nodes.insert(peer_node);
                        tracing::debug!(
                            node = %peer_node,
                            "Presence monitoring remote node for disconnect"
                        );
                    }

                    tracing::debug!(
                        peer = ?peer_pid,
                        "Requesting CRDT presence sync from remote Presence server"
                    );
                    let _ = crate::send_raw(peer_pid, msg_bytes.clone());
                }
            }
        }

        // Also monitor all currently connected nodes (in case we missed some)
        for node in crate::node::list(crate::node::ListOption::Connected) {
            if node != my_node
                && !self.monitored_nodes.contains(&node)
                && let Ok(_monitor_ref) = monitor_node(node)
            {
                self.monitored_nodes.insert(node);
                tracing::debug!(
                    node = %node,
                    "Presence monitoring connected node for disconnect"
                );
            }
        }

        Status::Ok
    }
}

#[async_trait]
impl HandleCall<Track> for Presence {
    type Reply = Option<PresenceRef>;
    type Output = Reply<Option<PresenceRef>>;

    async fn handle_call(&mut self, msg: Track, _from: From) -> Reply<Option<PresenceRef>> {
        // Track using CRDT
        let tag = self
            .crdt
            .track(&msg.topic, msg.pid, &msg.key, msg.meta.clone());

        tracing::debug!(
            topic = %msg.topic,
            key = %msg.key,
            pid = ?msg.pid,
            tag = ?tag,
            "Presence::track - added to CRDT state"
        );

        // Create PresenceRef from Tag
        let phx_ref = tag_to_ref(&tag);

        // Broadcast delta via PubSub (using old format for compatibility)
        let diff = PresenceDiff {
            joins: [(
                msg.key.clone(),
                PresenceState {
                    metas: vec![PresenceMeta {
                        phx_ref: phx_ref.clone(),
                        phx_ref_prev: None,
                        pid: msg.pid,
                        meta: msg.meta,
                        updated_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    }],
                },
            )]
            .into_iter()
            .collect(),
            leaves: HashMap::new(),
        };
        broadcast_delta(&self.pubsub, &self.name, &msg.topic, diff).await;

        // Also broadcast CRDT delta to other Presence servers
        if let Some(crdt_delta) = self.crdt.take_delta() {
            broadcast_crdt_delta(&self.pg_group, &msg.topic, crdt_delta).await;
        }

        Reply::Ok(Some(phx_ref))
    }
}

#[async_trait]
impl HandleCall<Untrack> for Presence {
    type Reply = Option<PresenceRef>;
    type Output = Reply<Option<PresenceRef>>;

    async fn handle_call(&mut self, msg: Untrack, _from: From) -> Reply<Option<PresenceRef>> {
        let removed_tags = self.crdt.untrack(&msg.topic, &msg.key, msg.pid);

        // Broadcast leave delta if we removed something
        if !removed_tags.is_empty() {
            // Get the entries we removed for the legacy format
            let metas: Vec<PresenceMeta> = removed_tags
                .iter()
                .map(|tag| PresenceMeta {
                    phx_ref: tag_to_ref(tag),
                    phx_ref_prev: None,
                    pid: msg.pid,
                    meta: vec![],
                    updated_at: 0,
                })
                .collect();

            let diff = PresenceDiff {
                joins: HashMap::new(),
                leaves: [(msg.key.clone(), PresenceState { metas })]
                    .into_iter()
                    .collect(),
            };
            broadcast_delta(&self.pubsub, &self.name, &msg.topic, diff).await;

            // Also broadcast CRDT delta
            if let Some(crdt_delta) = self.crdt.take_delta() {
                broadcast_crdt_delta(&self.pg_group, &msg.topic, crdt_delta).await;
            }
        }

        Reply::Ok(None)
    }
}

#[async_trait]
impl HandleCall<UntrackAll> for Presence {
    type Reply = Option<PresenceRef>;
    type Output = Reply<Option<PresenceRef>>;

    async fn handle_call(&mut self, msg: UntrackAll, _from: From) -> Reply<Option<PresenceRef>> {
        let removed = self.crdt.untrack_all(msg.pid);

        // Broadcast deltas per topic
        for (topic, entries) in &removed {
            let metas: Vec<PresenceMeta> = entries.iter().map(entry_to_meta).collect();

            if !metas.is_empty() {
                // Group by key
                let mut by_key: HashMap<String, Vec<PresenceMeta>> = HashMap::new();
                for entry in entries {
                    by_key
                        .entry(entry.key.clone())
                        .or_default()
                        .push(entry_to_meta(entry));
                }

                let diff = PresenceDiff {
                    joins: HashMap::new(),
                    leaves: by_key
                        .into_iter()
                        .map(|(k, metas)| (k, PresenceState { metas }))
                        .collect(),
                };
                broadcast_delta(&self.pubsub, &self.name, topic, diff).await;
            }
        }

        // Broadcast CRDT delta
        if let Some(crdt_delta) = self.crdt.take_delta() {
            // For untrack_all, we don't have a single topic, so broadcast to all
            for topic in removed.keys() {
                broadcast_crdt_delta(&self.pg_group, topic, crdt_delta.clone()).await;
            }
        }

        Reply::Ok(None)
    }
}

#[async_trait]
impl HandleCall<Update> for Presence {
    type Reply = Option<PresenceRef>;
    type Output = Reply<Option<PresenceRef>>;

    async fn handle_call(&mut self, msg: Update, _from: From) -> Reply<Option<PresenceRef>> {
        // Update is untrack + track
        let removed_tags = self.crdt.untrack(&msg.topic, &msg.key, msg.pid);
        let new_tag = self
            .crdt
            .track(&msg.topic, msg.pid, &msg.key, msg.meta.clone());
        let new_ref = tag_to_ref(&new_tag);

        // Broadcast join with previous ref
        let prev_ref = removed_tags.first().map(tag_to_ref);
        let diff = PresenceDiff {
            joins: [(
                msg.key.clone(),
                PresenceState {
                    metas: vec![PresenceMeta {
                        phx_ref: new_ref.clone(),
                        phx_ref_prev: prev_ref,
                        pid: msg.pid,
                        meta: msg.meta,
                        updated_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    }],
                },
            )]
            .into_iter()
            .collect(),
            leaves: HashMap::new(),
        };
        broadcast_delta(&self.pubsub, &self.name, &msg.topic, diff).await;

        // Broadcast CRDT delta
        if let Some(crdt_delta) = self.crdt.take_delta() {
            broadcast_crdt_delta(&self.pg_group, &msg.topic, crdt_delta).await;
        }

        Reply::Ok(if removed_tags.is_empty() {
            None
        } else {
            Some(new_ref)
        })
    }
}

#[async_trait]
impl HandleCall<List> for Presence {
    type Reply = PresenceListReply;
    type Output = Reply<PresenceListReply>;

    async fn handle_call(&mut self, msg: List, _from: From) -> Reply<PresenceListReply> {
        // Get presences from CRDT and convert to old format
        let crdt_presences = self.crdt.list(&msg.topic);
        let presences = crdt_to_presence_state(&crdt_presences);

        tracing::debug!(
            topic = %msg.topic,
            presence_count = presences.len(),
            keys = ?presences.keys().collect::<Vec<_>>(),
            "Presence::list called"
        );

        Reply::Ok(PresenceListReply(presences))
    }
}

#[async_trait]
impl HandleCall<Get> for Presence {
    type Reply = PresenceGetReply;
    type Output = Reply<PresenceGetReply>;

    async fn handle_call(&mut self, msg: Get, _from: From) -> Reply<PresenceGetReply> {
        let presence = self
            .crdt
            .get(&msg.topic, &msg.key)
            .map(|entries| PresenceState {
                metas: entries.iter().map(entry_to_meta).collect(),
            });
        Reply::Ok(PresenceGetReply(presence))
    }
}

#[async_trait]
impl HandleCast<ApplyDelta> for Presence {
    type Output = Status;

    async fn handle_cast(&mut self, msg: ApplyDelta) -> Status {
        // Convert old delta format to CRDT delta and merge
        let crdt_delta = presence_diff_to_crdt_delta(&msg.topic, &msg.diff);
        let _result = self.crdt.merge(&crdt_delta);
        Status::Ok
    }
}

#[async_trait]
impl HandleCast<SyncState> for Presence {
    type Output = Status;

    async fn handle_cast(&mut self, msg: SyncState) -> Status {
        // Convert old state format to CRDT delta and merge
        let crdt_delta = presence_state_to_crdt_delta(&msg.topic, &msg.state);
        let _result = self.crdt.merge(&crdt_delta);
        Status::Ok
    }
}

#[async_trait]
impl HandleInfo<CompleteInit> for Presence {
    type Output = Status;

    async fn handle_info(&mut self, _msg: CompleteInit) -> Status {
        self.complete_init().await
    }
}

// =============================================================================
// CRDT-specific message type
// =============================================================================

/// CRDT-based presence messages for synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum CrdtPresenceMessage {
    /// Delta update with CRDT semantics.
    Delta { topic: String, delta: CrdtDelta },
    /// Request sync with causal context.
    SyncRequest {
        from: Pid,
        context: HashMap<String, u64>,
    },
    /// Sync response with CRDT delta.
    SyncResponse { delta: CrdtDelta },
}

// =============================================================================
// Conversion functions
// =============================================================================

/// Convert a Tag to PresenceRef.
fn tag_to_ref(tag: &Tag) -> PresenceRef {
    PresenceRef::from_string(format!("{}:{}", tag.replica, tag.clock))
}

/// Convert a TrackedEntry to PresenceMeta.
fn entry_to_meta(entry: &TrackedEntry) -> PresenceMeta {
    PresenceMeta {
        phx_ref: tag_to_ref(&entry.tag),
        phx_ref_prev: None,
        pid: entry.pid,
        meta: entry.meta.clone(),
        updated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    }
}

/// Convert CRDT list result to old PresenceState format.
fn crdt_to_presence_state(
    crdt_result: &HashMap<String, Vec<TrackedEntry>>,
) -> HashMap<String, PresenceState> {
    crdt_result
        .iter()
        .map(|(key, entries)| {
            (
                key.clone(),
                PresenceState {
                    metas: entries.iter().map(entry_to_meta).collect(),
                },
            )
        })
        .collect()
}

/// Convert old PresenceDiff to CRDT delta.
fn presence_diff_to_crdt_delta(topic: &str, diff: &PresenceDiff) -> CrdtDelta {
    let mut delta = CrdtDelta::default();

    // Convert joins
    for (key, state) in &diff.joins {
        for meta in &state.metas {
            // Parse tag from phx_ref if possible, otherwise create a new one
            let tag = ref_to_tag(&meta.phx_ref);
            delta.joins.push(TrackedEntry {
                topic: topic.to_string(),
                pid: meta.pid,
                key: key.clone(),
                meta: meta.meta.clone(),
                tag,
            });
        }
    }

    // Convert leaves
    for state in diff.leaves.values() {
        for meta in &state.metas {
            delta.leave_tags.push(ref_to_tag(&meta.phx_ref));
        }
    }

    delta
}

/// Convert old PresenceState to CRDT delta.
fn presence_state_to_crdt_delta(topic: &str, state: &HashMap<String, PresenceState>) -> CrdtDelta {
    let mut delta = CrdtDelta::default();

    for (key, presence_state) in state {
        for meta in &presence_state.metas {
            let tag = ref_to_tag(&meta.phx_ref);
            delta.joins.push(TrackedEntry {
                topic: topic.to_string(),
                pid: meta.pid,
                key: key.clone(),
                meta: meta.meta.clone(),
                tag,
            });
        }
    }

    delta
}

/// Convert PresenceRef to Tag.
fn ref_to_tag(phx_ref: &PresenceRef) -> Tag {
    // Try to parse "replica:clock" format
    let ref_str = phx_ref.as_str();
    if let Some((replica, clock_str)) = ref_str.rsplit_once(':')
        && let Ok(clock) = clock_str.parse::<u64>()
    {
        return Tag::new(replica, clock);
    }

    // Fallback: use the ref as replica with clock 0
    // This handles legacy refs that aren't in our format
    Tag::new(ref_str, 0)
}

// =============================================================================
// Broadcasting functions
// =============================================================================

/// Broadcast a delta to other nodes via PubSub and to other Presence servers via pg.
async fn broadcast_delta(pubsub: &str, presence_name: &str, topic: &str, diff: PresenceDiff) {
    if diff.is_empty() {
        return;
    }

    let msg = PresenceMessage::Delta {
        topic: topic.to_string(),
        diff: diff.clone(),
    };

    // Broadcast to all subscribers of the presence topic (channels)
    let presence_topic = format!("presence:{}", topic);
    tracing::info!(
        topic = %topic,
        presence_topic = %presence_topic,
        joins = diff.joins.len(),
        leaves = diff.leaves.len(),
        "Broadcasting presence delta to subscribers"
    );
    let _ = PubSub::broadcast(pubsub, &presence_topic, &msg).await;

    // Note: We also send CRDT delta separately via broadcast_crdt_delta
    // The legacy format is for channel subscribers, CRDT format is for Presence servers
    let _ = presence_name; // Suppress unused warning
}

/// Broadcast CRDT delta to other Presence servers.
async fn broadcast_crdt_delta(pg_group: &str, topic: &str, delta: CrdtDelta) {
    if delta.is_empty() {
        return;
    }

    let msg = CrdtPresenceMessage::Delta {
        topic: topic.to_string(),
        delta,
    };

    let members = pg::get_members(pg_group);
    let self_pid = crate::current_pid();
    let my_node = crate::core::node::node_name_atom();

    if let Ok(msg_bytes) = postcard::to_allocvec(&msg) {
        for pid in members {
            // Skip ourselves and only send to remote nodes
            if pid != self_pid && pid.node() != my_node {
                tracing::trace!(
                    target_pid = ?pid,
                    topic = %topic,
                    "Forwarding CRDT presence delta to remote Presence server"
                );
                let _ = crate::send_raw(pid, msg_bytes.clone());
            }
        }
    }
}

// =============================================================================
// Public API - Convenience functions for interacting with Presence
// =============================================================================

impl Presence {
    /// Start a Presence server with the given configuration.
    pub async fn start_link(config: PresenceConfig) -> Result<Pid, String> {
        start::<Presence>(config)
            .await
            .map_err(|e| format!("failed to start Presence: {:?}", e))
    }

    /// Track a presence for the current process.
    pub async fn track<M: Serialize>(
        presence: &str,
        topic: &str,
        key: &str,
        meta: &M,
    ) -> Result<PresenceRef, String> {
        Self::track_pid(presence, topic, key, crate::current_pid(), meta).await
    }

    /// Track a presence for a specific PID.
    pub async fn track_pid<M: Serialize>(
        presence: &str,
        topic: &str,
        key: &str,
        pid: Pid,
        meta: &M,
    ) -> Result<PresenceRef, String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let meta_bytes =
            postcard::to_allocvec(meta).map_err(|e| format!("serialize failed: {}", e))?;

        let request = Track {
            topic: topic.to_string(),
            key: key.to_string(),
            pid,
            meta: meta_bytes,
        };

        match call::<Track, Option<PresenceRef>>(server_pid, request, Duration::from_secs(5)).await
        {
            Ok(Some(phx_ref)) => Ok(phx_ref),
            Ok(None) => Err("track returned no ref".to_string()),
            Err(e) => Err(format!("track failed: {:?}", e)),
        }
    }

    /// Untrack a presence for the current process.
    pub async fn untrack(presence: &str, topic: &str, key: &str) -> Result<(), String> {
        Self::untrack_pid(presence, topic, key, crate::current_pid()).await
    }

    /// Untrack a presence for a specific PID.
    pub async fn untrack_pid(
        presence: &str,
        topic: &str,
        key: &str,
        pid: Pid,
    ) -> Result<(), String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = Untrack {
            topic: topic.to_string(),
            key: key.to_string(),
            pid,
        };

        call::<Untrack, Option<PresenceRef>>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("untrack failed: {:?}", e))?;

        Ok(())
    }

    /// Untrack all presences for a PID.
    pub async fn untrack_all(presence: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = UntrackAll { pid };

        call::<UntrackAll, Option<PresenceRef>>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("untrack_all failed: {:?}", e))?;

        Ok(())
    }

    /// Update presence metadata for the current process.
    pub async fn update<M: Serialize>(
        presence: &str,
        topic: &str,
        key: &str,
        meta: &M,
    ) -> Result<Option<PresenceRef>, String> {
        Self::update_pid(presence, topic, key, crate::current_pid(), meta).await
    }

    /// Update presence metadata for a specific PID.
    pub async fn update_pid<M: Serialize>(
        presence: &str,
        topic: &str,
        key: &str,
        pid: Pid,
        meta: &M,
    ) -> Result<Option<PresenceRef>, String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let meta_bytes =
            postcard::to_allocvec(meta).map_err(|e| format!("serialize failed: {}", e))?;

        let request = Update {
            topic: topic.to_string(),
            key: key.to_string(),
            pid,
            meta: meta_bytes,
        };

        call::<Update, Option<PresenceRef>>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("update failed: {:?}", e))
    }

    /// List all presences for a topic.
    pub async fn list(
        presence: &str,
        topic: &str,
    ) -> Result<HashMap<String, PresenceState>, String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = List {
            topic: topic.to_string(),
        };

        match call::<List, PresenceListReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PresenceListReply(presences)) => Ok(presences),
            Err(e) => Err(format!("list failed: {:?}", e)),
        }
    }

    /// Get presence for a specific key.
    pub async fn get(
        presence: &str,
        topic: &str,
        key: &str,
    ) -> Result<Option<PresenceState>, String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = Get {
            topic: topic.to_string(),
            key: key.to_string(),
        };

        match call::<Get, PresenceGetReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PresenceGetReply(presence_state)) => Ok(presence_state),
            Err(e) => Err(format!("get failed: {:?}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presence_config() {
        let config = PresenceConfig::new("test", "pubsub");
        assert_eq!(config.name, "test");
        assert_eq!(config.pubsub, "pubsub");
    }

    #[test]
    fn test_presence_diff_empty() {
        let diff = PresenceDiff::default();
        assert!(diff.is_empty());
    }

    #[test]
    fn test_tag_to_ref_conversion() {
        let tag = Tag::new("node1@localhost", 42);
        let phx_ref = tag_to_ref(&tag);
        assert!(phx_ref.as_str().contains("node1@localhost"));
        assert!(phx_ref.as_str().contains("42"));
    }

    #[test]
    fn test_ref_to_tag_roundtrip() {
        let original_tag = Tag::new("node1@localhost", 123);
        let phx_ref = tag_to_ref(&original_tag);
        let recovered_tag = ref_to_tag(&phx_ref);

        assert_eq!(original_tag.replica, recovered_tag.replica);
        assert_eq!(original_tag.clock, recovered_tag.clock);
    }
}
