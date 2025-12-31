//! Presence GenServer implementation using enum-based dispatch.

use super::crdt::{PresenceCrdt, PresenceDelta as CrdtDelta, Tag, TrackedEntry};
use super::types::{PresenceDiff, PresenceMessage, PresenceMeta, PresenceRef, PresenceState};
use crate::core::{Atom, DecodeError, Pid};
use crate::dist::pg;
use crate::distribution::{NodeDown, monitor_node};
use crate::gen_server::{
    ExitReason, From, GenServer, Init, Reply, Status, async_trait, call, start,
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
// Call Message Enum
// =============================================================================

/// All call (request/response) messages for Presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceCall {
    /// Track a presence.
    Track {
        /// The topic to track presence on.
        topic: String,
        /// The key (usually user ID) to track.
        key: String,
        /// The PID being tracked.
        pid: Pid,
        /// Serialized metadata.
        meta: Vec<u8>,
    },
    /// Untrack a presence.
    Untrack {
        /// The topic.
        topic: String,
        /// The key.
        key: String,
        /// The PID to untrack.
        pid: Pid,
    },
    /// Untrack all presences for a PID.
    UntrackAll {
        /// The PID to untrack.
        pid: Pid,
    },
    /// Update presence metadata.
    Update {
        /// The topic.
        topic: String,
        /// The key.
        key: String,
        /// The PID.
        pid: Pid,
        /// New serialized metadata.
        meta: Vec<u8>,
    },
    /// List presences for a topic.
    List {
        /// The topic to list.
        topic: String,
    },
    /// Get presence for a specific key.
    Get {
        /// The topic.
        topic: String,
        /// The key.
        key: String,
    },
}

impl Message for PresenceCall {
    const TAG: &'static str = "PresenceCall";

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::TAG, &encode_payload(self))
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

/// All cast (fire-and-forget) messages for Presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceCast {
    /// Apply a delta from another node.
    ApplyDelta {
        /// The topic.
        topic: String,
        /// The presence diff.
        diff: PresenceDiff,
    },
    /// Sync full state from another node.
    SyncState {
        /// The topic.
        topic: String,
        /// The full state.
        state: HashMap<String, PresenceState>,
    },
}

impl Message for PresenceCast {
    const TAG: &'static str = "PresenceCast";

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::TAG, &encode_payload(self))
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

/// Info messages for Presence (system events, continue messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceInfo {
    /// Complete initialization after we have our PID.
    CompleteInit,
    /// CRDT delta from another node.
    CrdtDelta {
        /// The topic.
        topic: String,
        /// The CRDT delta.
        delta: CrdtDelta,
    },
    /// Request sync with causal context.
    CrdtSyncRequest {
        /// The requesting PID.
        from: Pid,
        /// The causal context.
        context: HashMap<String, u64>,
    },
    /// Sync response with CRDT delta.
    CrdtSyncResponse {
        /// The delta to apply.
        delta: CrdtDelta,
    },
    /// Node disconnected.
    NodeDown {
        /// The node name.
        node_name: String,
        /// The reason.
        reason: String,
    },
}

impl Message for PresenceInfo {
    const TAG: &'static str = "PresenceInfo";

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::TAG, &encode_payload(self))
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

/// Reply type for Presence calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceReply {
    /// Acknowledgment, optionally with a reference.
    Ok(Option<PresenceRef>),
    /// List of presences by key.
    PresenceList(HashMap<String, PresenceState>),
    /// Single presence state.
    PresenceGet(Option<PresenceState>),
}

impl Message for PresenceReply {
    const TAG: &'static str = "PresenceReply";

    fn encode_local(&self) -> Vec<u8> {
        encode_with_tag(Self::TAG, &encode_payload(self))
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

#[async_trait]
impl GenServer for Presence {
    type Args = PresenceConfig;
    type Call = PresenceCall;
    type Cast = PresenceCast;
    type Info = PresenceInfo;
    type Reply = PresenceReply;

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
        Init::Continue(server, PresenceInfo::CompleteInit.encode_local())
    }

    async fn handle_call(&mut self, msg: PresenceCall, _from: From) -> Reply<PresenceReply> {
        match msg {
            PresenceCall::Track {
                topic,
                key,
                pid,
                meta,
            } => {
                // Track using CRDT
                let tag = self.crdt.track(&topic, pid, &key, meta.clone());

                tracing::debug!(
                    topic = %topic,
                    key = %key,
                    pid = ?pid,
                    tag = ?tag,
                    "Presence::track - added to CRDT state"
                );

                // Create PresenceRef from Tag
                let phx_ref = tag_to_ref(&tag);

                // Broadcast delta via PubSub (using old format for compatibility)
                let diff = PresenceDiff {
                    joins: [(
                        key.clone(),
                        PresenceState {
                            metas: vec![PresenceMeta {
                                phx_ref: phx_ref.clone(),
                                phx_ref_prev: None,
                                pid,
                                meta,
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
                broadcast_delta(&self.pubsub, &self.name, &topic, diff).await;

                // Also broadcast CRDT delta to other Presence servers
                if let Some(crdt_delta) = self.crdt.take_delta() {
                    broadcast_crdt_delta(&self.pg_group, &topic, crdt_delta).await;
                }

                Reply::Ok(PresenceReply::Ok(Some(phx_ref)))
            }
            PresenceCall::Untrack { topic, key, pid } => {
                let removed_tags = self.crdt.untrack(&topic, &key, pid);

                // Broadcast leave delta if we removed something
                if !removed_tags.is_empty() {
                    // Get the entries we removed for the legacy format
                    let metas: Vec<PresenceMeta> = removed_tags
                        .iter()
                        .map(|tag| PresenceMeta {
                            phx_ref: tag_to_ref(tag),
                            phx_ref_prev: None,
                            pid,
                            meta: vec![],
                            updated_at: 0,
                        })
                        .collect();

                    let diff = PresenceDiff {
                        joins: HashMap::new(),
                        leaves: [(key.clone(), PresenceState { metas })]
                            .into_iter()
                            .collect(),
                    };
                    broadcast_delta(&self.pubsub, &self.name, &topic, diff).await;

                    // Also broadcast CRDT delta
                    if let Some(crdt_delta) = self.crdt.take_delta() {
                        broadcast_crdt_delta(&self.pg_group, &topic, crdt_delta).await;
                    }
                }

                Reply::Ok(PresenceReply::Ok(None))
            }
            PresenceCall::UntrackAll { pid } => {
                let removed = self.crdt.untrack_all(pid);

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

                Reply::Ok(PresenceReply::Ok(None))
            }
            PresenceCall::Update {
                topic,
                key,
                pid,
                meta,
            } => {
                // Update is untrack + track
                let removed_tags = self.crdt.untrack(&topic, &key, pid);
                let new_tag = self.crdt.track(&topic, pid, &key, meta.clone());
                let new_ref = tag_to_ref(&new_tag);

                // Broadcast join with previous ref
                let prev_ref = removed_tags.first().map(tag_to_ref);
                let diff = PresenceDiff {
                    joins: [(
                        key.clone(),
                        PresenceState {
                            metas: vec![PresenceMeta {
                                phx_ref: new_ref.clone(),
                                phx_ref_prev: prev_ref,
                                pid,
                                meta,
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
                broadcast_delta(&self.pubsub, &self.name, &topic, diff).await;

                // Broadcast CRDT delta
                if let Some(crdt_delta) = self.crdt.take_delta() {
                    broadcast_crdt_delta(&self.pg_group, &topic, crdt_delta).await;
                }

                Reply::Ok(PresenceReply::Ok(if removed_tags.is_empty() {
                    None
                } else {
                    Some(new_ref)
                }))
            }
            PresenceCall::List { topic } => {
                // Get presences from CRDT and convert to old format
                let crdt_presences = self.crdt.list(&topic);
                let presences = crdt_to_presence_state(&crdt_presences);

                tracing::debug!(
                    topic = %topic,
                    presence_count = presences.len(),
                    keys = ?presences.keys().collect::<Vec<_>>(),
                    "Presence::list called"
                );

                Reply::Ok(PresenceReply::PresenceList(presences))
            }
            PresenceCall::Get { topic, key } => {
                let presence = self.crdt.get(&topic, &key).map(|entries| PresenceState {
                    metas: entries.iter().map(entry_to_meta).collect(),
                });
                Reply::Ok(PresenceReply::PresenceGet(presence))
            }
        }
    }

    async fn handle_cast(&mut self, msg: PresenceCast) -> Status {
        match msg {
            PresenceCast::ApplyDelta { topic, diff } => {
                // Convert old delta format to CRDT delta and merge
                let crdt_delta = presence_diff_to_crdt_delta(&topic, &diff);
                let _result = self.crdt.merge(&crdt_delta);
                Status::Ok
            }
            PresenceCast::SyncState { topic, state } => {
                // Convert old state format to CRDT delta and merge
                let crdt_delta = presence_state_to_crdt_delta(&topic, &state);
                let _result = self.crdt.merge(&crdt_delta);
                Status::Ok
            }
        }
    }

    async fn handle_info(&mut self, msg: PresenceInfo) -> Status {
        match msg {
            PresenceInfo::CompleteInit => self.complete_init().await,
            PresenceInfo::CrdtDelta { topic, delta } => self.handle_crdt_delta(topic, delta).await,
            PresenceInfo::CrdtSyncRequest { from, context } => {
                self.handle_crdt_sync_request(from, context).await
            }
            PresenceInfo::CrdtSyncResponse { delta } => self.handle_crdt_sync_response(delta).await,
            PresenceInfo::NodeDown { node_name, reason } => {
                self.handle_node_down_info(node_name, reason).await
            }
        }
    }

    async fn handle_continue(&mut self, arg: Vec<u8>) -> Status {
        // Try to decode as PresenceInfo (for continue messages)
        if let Ok((_tag, payload)) = crate::message::decode_tag(&arg)
            && let Ok(info) = PresenceInfo::decode_local(payload)
        {
            return self.handle_info(info).await;
        }

        // Also check for raw messages that might be system messages
        self.handle_raw_message(arg).await
    }

    async fn terminate(&mut self, _reason: ExitReason) {
        // Clean up: leave pg group
        if let Some(pid) = self.self_pid {
            pg::leave(&self.pg_group, pid);
        }
    }
}

impl Presence {
    /// Handle CRDT delta from a remote node.
    async fn handle_crdt_delta(&mut self, topic: String, delta: CrdtDelta) -> Status {
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
                    let response = PresenceInfo::CrdtDelta {
                        topic: topic.clone(),
                        delta: our_delta,
                    };
                    // Find their presence server PID
                    for peer_pid in pg::get_members(&self.pg_group) {
                        if peer_pid.node() == replica {
                            tracing::debug!(
                                peer = ?peer_pid,
                                "Sending our presence state to new remote node"
                            );
                            let _ = crate::send(peer_pid, &response);
                            break;
                        }
                    }
                }
            }
        }

        let _result = self.crdt.merge(&delta);
        Status::Ok
    }

    /// Handle CRDT sync request from a remote node.
    async fn handle_crdt_sync_request(
        &mut self,
        from: Pid,
        context: HashMap<String, u64>,
    ) -> Status {
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
            let response = PresenceInfo::CrdtSyncResponse { delta };
            let _ = crate::send(from, &response);
        }

        Status::Ok
    }

    /// Handle CRDT sync response from a remote node.
    async fn handle_crdt_sync_response(&mut self, delta: CrdtDelta) -> Status {
        tracing::debug!(
            joins = delta.joins.len(),
            "Presence received CRDT sync response"
        );
        let _result = self.crdt.merge(&delta);
        Status::Ok
    }

    /// Handle node down notification via PresenceInfo.
    async fn handle_node_down_info(&mut self, node_name: String, reason: String) -> Status {
        tracing::info!(
            node = %node_name,
            reason = %reason,
            "Presence detected node disconnect, removing presences"
        );

        // Remove all presences from the disconnected node
        let removed = self.crdt.remove_down_replica(&node_name);
        self.monitored_nodes.remove(&Atom::new(&node_name));

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

    /// Handle raw messages that aren't typed (NodeDown, CRDT, legacy).
    async fn handle_raw_message(&mut self, payload: Vec<u8>) -> Status {
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

        // Try to decode as legacy CompleteInit (tag-based)
        if let Ok((tag, _)) = crate::message::decode_tag(&payload)
            && tag == "PresenceCompleteInit"
        {
            return self.complete_init().await;
        }

        Status::Ok
    }

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
        // Handle legacy CRDT messages (backwards compatibility)
        // New code uses PresenceInfo variants, but we still handle raw postcard messages
        match msg {
            CrdtPresenceMessage::Delta { topic, delta } => {
                // Delegate to the new handler
                self.handle_crdt_delta(topic, delta).await
            }
            CrdtPresenceMessage::SyncRequest { from, context } => {
                // Delegate to the new handler
                self.handle_crdt_sync_request(from, context).await
            }
            CrdtPresenceMessage::SyncResponse { delta } => {
                // Delegate to the new handler
                self.handle_crdt_sync_response(delta).await
            }
        }
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

                // Send full state using new PresenceInfo format
                let delta = self.crdt.extract_all();

                if !delta.is_empty() {
                    let response = PresenceInfo::CrdtSyncResponse { delta };
                    let _ = crate::send(from, &response);
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
        let sync_request = PresenceInfo::CrdtSyncRequest {
            from: self_pid,
            context: self.crdt.context().clone(),
        };
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
                let _ = crate::send(peer_pid, &sync_request);
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

// =============================================================================
// CRDT-specific message type
// =============================================================================

/// CRDT-based presence messages for synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum CrdtPresenceMessage {
    /// Delta update with CRDT semantics.
    Delta {
        /// The topic.
        topic: String,
        /// The CRDT delta.
        delta: CrdtDelta,
    },
    /// Request sync with causal context.
    SyncRequest {
        /// The requesting PID.
        from: Pid,
        /// The causal context.
        context: HashMap<String, u64>,
    },
    /// Sync response with CRDT delta.
    SyncResponse {
        /// The delta to apply.
        delta: CrdtDelta,
    },
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

    let msg = PresenceInfo::CrdtDelta {
        topic: topic.to_string(),
        delta,
    };

    let members = pg::get_members(pg_group);
    let self_pid = crate::current_pid();
    let my_node = crate::core::node::node_name_atom();

    for pid in members {
        // Skip ourselves and only send to remote nodes
        if pid != self_pid && pid.node() != my_node {
            tracing::trace!(
                target_pid = ?pid,
                topic = %topic,
                "Forwarding CRDT presence delta to remote Presence server"
            );
            let _ = crate::send(pid, &msg);
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

        let request = PresenceCall::Track {
            topic: topic.to_string(),
            key: key.to_string(),
            pid,
            meta: meta_bytes,
        };

        match call::<Presence, PresenceReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PresenceReply::Ok(Some(phx_ref))) => Ok(phx_ref),
            Ok(PresenceReply::Ok(None)) => Err("track returned no ref".to_string()),
            Ok(_) => Err("unexpected reply type".to_string()),
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

        let request = PresenceCall::Untrack {
            topic: topic.to_string(),
            key: key.to_string(),
            pid,
        };

        call::<Presence, PresenceReply>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("untrack failed: {:?}", e))?;

        Ok(())
    }

    /// Untrack all presences for a PID.
    pub async fn untrack_all(presence: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = PresenceCall::UntrackAll { pid };

        call::<Presence, PresenceReply>(server_pid, request, Duration::from_secs(5))
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

        let request = PresenceCall::Update {
            topic: topic.to_string(),
            key: key.to_string(),
            pid,
            meta: meta_bytes,
        };

        match call::<Presence, PresenceReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PresenceReply::Ok(phx_ref)) => Ok(phx_ref),
            Ok(_) => Err("unexpected reply type".to_string()),
            Err(e) => Err(format!("update failed: {:?}", e)),
        }
    }

    /// List all presences for a topic.
    pub async fn list(
        presence: &str,
        topic: &str,
    ) -> Result<HashMap<String, PresenceState>, String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = PresenceCall::List {
            topic: topic.to_string(),
        };

        match call::<Presence, PresenceReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PresenceReply::PresenceList(presences)) => Ok(presences),
            Ok(_) => Err("unexpected reply type".to_string()),
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

        let request = PresenceCall::Get {
            topic: topic.to_string(),
            key: key.to_string(),
        };

        match call::<Presence, PresenceReply>(server_pid, request, Duration::from_secs(5)).await {
            Ok(PresenceReply::PresenceGet(presence_state)) => Ok(presence_state),
            Ok(_) => Err("unexpected reply type".to_string()),
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
}
