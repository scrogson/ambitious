//! Presence GenServer implementation.

use super::types::{PresenceDiff, PresenceMessage, PresenceMeta, PresenceRef, PresenceState};
use crate::core::{Pid, RawTerm};
use crate::dist::pg;
use crate::gen_server::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, GenServer, InfoResult, InitResult,
    async_trait,
};
use crate::pubsub::PubSub;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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

/// Messages that can be sent to the Presence server via call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceCall {
    /// Track a presence.
    Track {
        topic: String,
        key: String,
        pid: Pid,
        meta: Vec<u8>,
    },
    /// Untrack a presence.
    Untrack {
        topic: String,
        key: String,
        pid: Pid,
    },
    /// Untrack all presences for a PID.
    UntrackAll { pid: Pid },
    /// Update presence metadata.
    Update {
        topic: String,
        key: String,
        pid: Pid,
        meta: Vec<u8>,
    },
    /// List presences for a topic.
    List { topic: String },
    /// Get presence for a specific key.
    Get { topic: String, key: String },
}

/// Messages that can be sent via cast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceCast {
    /// Apply a delta from another node.
    ApplyDelta { topic: String, diff: PresenceDiff },
    /// Sync full state from another node.
    SyncState {
        topic: String,
        state: HashMap<String, PresenceState>,
    },
}

/// Reply from Presence calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresenceReply {
    /// Operation succeeded, returns the presence ref.
    Ok(Option<PresenceRef>),
    /// List of presences.
    List(HashMap<String, PresenceState>),
    /// Single presence state.
    Get(Option<PresenceState>),
}

/// Internal state of the Presence server.
pub struct PresenceServerState {
    /// The name this Presence is registered under.
    name: String,
    /// The PubSub server name.
    pubsub: String,
    /// The pg group for presence servers.
    pg_group: String,
    /// Presence state per topic: topic -> (key -> state).
    state: Arc<DashMap<String, HashMap<String, PresenceState>>>,
    /// Reverse lookup: PID -> set of (topic, key) pairs.
    pid_to_presence: Arc<DashMap<Pid, HashSet<(String, String)>>>,
    /// Our own PID.
    self_pid: Option<Pid>,
}

impl Default for PresenceServerState {
    fn default() -> Self {
        Self {
            name: String::new(),
            pubsub: String::new(),
            pg_group: String::new(),
            state: Arc::new(DashMap::new()),
            pid_to_presence: Arc::new(DashMap::new()),
            self_pid: None,
        }
    }
}

/// The Presence GenServer.
pub struct Presence;

#[async_trait]
impl GenServer for Presence {
    type State = PresenceServerState;
    type InitArg = PresenceConfig;
    type Call = PresenceCall;
    type Cast = PresenceCast;
    type Reply = PresenceReply;

    async fn init(config: PresenceConfig) -> InitResult<PresenceServerState> {
        let pg_group = format!("presence:{}", config.name);
        let state = PresenceServerState {
            name: config.name,
            pubsub: config.pubsub,
            pg_group,
            state: Arc::new(DashMap::new()),
            pid_to_presence: Arc::new(DashMap::new()),
            self_pid: None,
        };

        // Register and subscribe in handle_continue
        InitResult::ok_continue(state, &())
    }

    async fn handle_call(
        request: PresenceCall,
        _from: From,
        state: &mut PresenceServerState,
    ) -> CallResult<PresenceServerState, PresenceReply> {
        match request {
            PresenceCall::Track {
                topic,
                key,
                pid,
                meta,
            } => {
                let presence_meta = PresenceMeta {
                    phx_ref: PresenceRef::new(),
                    phx_ref_prev: None,
                    pid,
                    meta,
                    updated_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                };
                let phx_ref = presence_meta.phx_ref.clone();

                // Add to state
                {
                    let mut topic_state = state.state.entry(topic.clone()).or_default();
                    let key_state = topic_state.entry(key.clone()).or_default();
                    key_state.metas.push(presence_meta.clone());
                    tracing::debug!(
                        topic = %topic,
                        key = %key,
                        pid = ?pid,
                        total_keys = topic_state.len(),
                        "Presence::track - added to state"
                    );
                }

                // Track reverse lookup
                state
                    .pid_to_presence
                    .entry(pid)
                    .or_default()
                    .insert((topic.clone(), key.clone()));

                // Broadcast delta via PubSub
                let diff = PresenceDiff {
                    joins: [(
                        key,
                        PresenceState {
                            metas: vec![presence_meta],
                        },
                    )]
                    .into_iter()
                    .collect(),
                    leaves: HashMap::new(),
                };
                broadcast_delta(&state.pubsub, &state.name, &topic, diff).await;

                CallResult::Reply(PresenceReply::Ok(Some(phx_ref)), std::mem::take(state))
            }

            PresenceCall::Untrack { topic, key, pid } => {
                let mut removed_metas = Vec::new();

                // Remove from state
                if let Some(mut topic_state) = state.state.get_mut(&topic)
                    && let Some(key_state) = topic_state.get_mut(&key)
                {
                    removed_metas = key_state
                        .metas
                        .iter()
                        .filter(|m| m.pid == pid)
                        .cloned()
                        .collect();
                    key_state.metas.retain(|m| m.pid != pid);

                    if key_state.metas.is_empty() {
                        topic_state.remove(&key);
                    }
                }

                // Update reverse lookup
                if let Some(mut presences) = state.pid_to_presence.get_mut(&pid) {
                    presences.remove(&(topic.clone(), key.clone()));
                }

                // Broadcast delta if we removed something
                if !removed_metas.is_empty() {
                    let diff = PresenceDiff {
                        joins: HashMap::new(),
                        leaves: [(
                            key,
                            PresenceState {
                                metas: removed_metas,
                            },
                        )]
                        .into_iter()
                        .collect(),
                    };
                    broadcast_delta(&state.pubsub, &state.name, &topic, diff).await;
                }

                CallResult::Reply(PresenceReply::Ok(None), std::mem::take(state))
            }

            PresenceCall::UntrackAll { pid } => {
                if let Some((_, presences)) = state.pid_to_presence.remove(&pid) {
                    let mut by_topic: HashMap<String, Vec<(String, Vec<PresenceMeta>)>> =
                        HashMap::new();

                    for (topic, key) in presences {
                        let mut removed_metas = Vec::new();

                        if let Some(mut topic_state) = state.state.get_mut(&topic)
                            && let Some(key_state) = topic_state.get_mut(&key)
                        {
                            removed_metas = key_state
                                .metas
                                .iter()
                                .filter(|m| m.pid == pid)
                                .cloned()
                                .collect();
                            key_state.metas.retain(|m| m.pid != pid);

                            if key_state.metas.is_empty() {
                                topic_state.remove(&key);
                            }
                        }

                        if !removed_metas.is_empty() {
                            by_topic
                                .entry(topic)
                                .or_default()
                                .push((key, removed_metas));
                        }
                    }

                    // Broadcast deltas per topic
                    for (topic, leaves) in by_topic {
                        let diff = PresenceDiff {
                            joins: HashMap::new(),
                            leaves: leaves
                                .into_iter()
                                .map(|(k, metas)| (k, PresenceState { metas }))
                                .collect(),
                        };
                        broadcast_delta(&state.pubsub, &state.name, &topic, diff).await;
                    }
                }

                CallResult::Reply(PresenceReply::Ok(None), std::mem::take(state))
            }

            PresenceCall::Update {
                topic,
                key,
                pid,
                meta,
            } => {
                let mut new_ref = None;

                if let Some(mut topic_state) = state.state.get_mut(&topic)
                    && let Some(key_state) = topic_state.get_mut(&key)
                {
                    for m in &mut key_state.metas {
                        if m.pid == pid {
                            let new_meta = PresenceMeta {
                                phx_ref: PresenceRef::new(),
                                phx_ref_prev: Some(m.phx_ref.clone()),
                                pid,
                                meta: meta.clone(),
                                updated_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            };
                            new_ref = Some(new_meta.phx_ref.clone());

                            // Broadcast the update
                            let diff = PresenceDiff {
                                joins: [(
                                    key.clone(),
                                    PresenceState {
                                        metas: vec![new_meta.clone()],
                                    },
                                )]
                                .into_iter()
                                .collect(),
                                leaves: HashMap::new(),
                            };

                            *m = new_meta;
                            broadcast_delta(&state.pubsub, &state.name, &topic, diff).await;
                            break;
                        }
                    }
                }

                CallResult::Reply(PresenceReply::Ok(new_ref), std::mem::take(state))
            }

            PresenceCall::List { topic } => {
                let presences = state
                    .state
                    .get(&topic)
                    .map(|s| s.clone())
                    .unwrap_or_default();
                tracing::debug!(
                    topic = %topic,
                    presence_count = presences.len(),
                    keys = ?presences.keys().collect::<Vec<_>>(),
                    "Presence::list called"
                );
                CallResult::Reply(PresenceReply::List(presences), std::mem::take(state))
            }

            PresenceCall::Get { topic, key } => {
                let presence = state.state.get(&topic).and_then(|s| s.get(&key).cloned());
                CallResult::Reply(PresenceReply::Get(presence), std::mem::take(state))
            }
        }
    }

    async fn handle_cast(
        msg: PresenceCast,
        state: &mut PresenceServerState,
    ) -> CastResult<PresenceServerState> {
        match msg {
            PresenceCast::ApplyDelta { topic, diff } => {
                apply_delta(&state.state, &topic, diff);
                CastResult::NoReply(std::mem::take(state))
            }
            PresenceCast::SyncState {
                topic,
                state: remote_state,
            } => {
                merge_state(&state.state, &topic, remote_state);
                CastResult::NoReply(std::mem::take(state))
            }
        }
    }

    async fn handle_info(
        msg: RawTerm,
        state: &mut PresenceServerState,
    ) -> InfoResult<PresenceServerState> {
        // Handle presence messages from other Presence servers
        if let Ok(presence_msg) = postcard::from_bytes::<PresenceMessage>(msg.as_ref()) {
            match presence_msg {
                PresenceMessage::Delta { topic, diff } => {
                    tracing::trace!(
                        topic = %topic,
                        joins = diff.joins.len(),
                        leaves = diff.leaves.len(),
                        "Presence received delta from remote node, applying"
                    );
                    apply_delta(&state.state, &topic, diff);
                }
                PresenceMessage::StateSync {
                    topic,
                    state: remote_state,
                } => {
                    tracing::trace!(
                        topic = %topic,
                        remote_keys = remote_state.len(),
                        "Presence received state sync from remote node, merging"
                    );
                    merge_state(&state.state, &topic, remote_state);
                }
            }
        }
        InfoResult::NoReply(std::mem::take(state))
    }

    async fn handle_continue(
        _arg: ContinueArg,
        state: &mut PresenceServerState,
    ) -> ContinueResult<PresenceServerState> {
        let self_pid = crate::current_pid();
        state.self_pid = Some(self_pid);

        // Register ourselves by name
        let _ = crate::register(&state.name, self_pid);

        // Join the pg group so other Presence servers can find us
        pg::join(&state.pg_group, self_pid);

        tracing::debug!(
            name = %state.name,
            pubsub = %state.pubsub,
            pg_group = %state.pg_group,
            pid = ?self_pid,
            "Presence started"
        );

        ContinueResult::NoReply(std::mem::take(state))
    }
}

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
    let _ = PubSub::broadcast(pubsub, &presence_topic, &msg).await;

    // Also send to other Presence servers in the pg group so they can merge state
    // The pg group name is "presence:{presence_name}" (e.g., "presence:chat_presence")
    let pg_group = format!("presence:{}", presence_name);
    let members = pg::get_members(&pg_group);
    let self_pid = crate::current_pid();
    let my_node = crate::core::node::node_name_atom();

    if let Ok(msg_bytes) = postcard::to_allocvec(&msg) {
        for pid in members {
            // Skip ourselves and only send to remote nodes
            if pid != self_pid && pid.node() != my_node {
                tracing::trace!(
                    target_pid = ?pid,
                    topic = %topic,
                    "Forwarding presence delta to remote Presence server"
                );
                let _ = crate::send_raw(pid, msg_bytes.clone());
            }
        }
    }
}

/// Apply a delta to local state.
fn apply_delta(
    state: &DashMap<String, HashMap<String, PresenceState>>,
    topic: &str,
    diff: PresenceDiff,
) {
    let mut topic_state = state.entry(topic.to_string()).or_default();

    // Process leaves first
    for (key, leave_state) in diff.leaves {
        if let Some(local_state) = topic_state.get_mut(&key) {
            for leave_meta in &leave_state.metas {
                local_state
                    .metas
                    .retain(|m| m.phx_ref != leave_meta.phx_ref);
            }
            if local_state.metas.is_empty() {
                topic_state.remove(&key);
            }
        }
    }

    // Process joins
    for (key, join_state) in diff.joins {
        let local_state = topic_state.entry(key).or_default();
        for join_meta in join_state.metas {
            // Remove old version if this is an update
            if let Some(ref prev) = join_meta.phx_ref_prev {
                local_state.metas.retain(|m| &m.phx_ref != prev);
            }
            // Add new meta if not already present
            let exists = local_state
                .metas
                .iter()
                .any(|m| m.phx_ref == join_meta.phx_ref);
            if !exists {
                local_state.metas.push(join_meta);
            }
        }
    }
}

/// Merge remote state into local state.
fn merge_state(
    state: &DashMap<String, HashMap<String, PresenceState>>,
    topic: &str,
    remote: HashMap<String, PresenceState>,
) {
    let mut topic_state = state.entry(topic.to_string()).or_default();

    for (key, remote_state) in remote {
        let local_state = topic_state.entry(key).or_default();

        // Merge metas - add any we don't have
        for remote_meta in remote_state.metas {
            let exists = local_state
                .metas
                .iter()
                .any(|m| m.phx_ref == remote_meta.phx_ref);
            if !exists {
                local_state.metas.push(remote_meta);
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
        crate::gen_server::start::<Presence>(config)
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

        match crate::gen_server::call::<Presence>(server_pid, request, Duration::from_secs(5)).await
        {
            Ok(PresenceReply::Ok(Some(phx_ref))) => Ok(phx_ref),
            Ok(_) => Err("unexpected reply".to_string()),
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

        crate::gen_server::call::<Presence>(server_pid, request, Duration::from_secs(5))
            .await
            .map_err(|e| format!("untrack failed: {:?}", e))?;

        Ok(())
    }

    /// Untrack all presences for a PID.
    pub async fn untrack_all(presence: &str, pid: Pid) -> Result<(), String> {
        let server_pid =
            crate::whereis(presence).ok_or_else(|| format!("Presence '{}' not found", presence))?;

        let request = PresenceCall::UntrackAll { pid };

        crate::gen_server::call::<Presence>(server_pid, request, Duration::from_secs(5))
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

        match crate::gen_server::call::<Presence>(server_pid, request, Duration::from_secs(5)).await
        {
            Ok(PresenceReply::Ok(phx_ref)) => Ok(phx_ref),
            Ok(_) => Err("unexpected reply".to_string()),
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

        match crate::gen_server::call::<Presence>(server_pid, request, Duration::from_secs(5)).await
        {
            Ok(PresenceReply::List(presences)) => Ok(presences),
            Ok(_) => Err("unexpected reply".to_string()),
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

        match crate::gen_server::call::<Presence>(server_pid, request, Duration::from_secs(5)).await
        {
            Ok(PresenceReply::Get(presence_state)) => Ok(presence_state),
            Ok(_) => Err("unexpected reply".to_string()),
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
}
