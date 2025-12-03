//! PubSub implementation using Registry + pg.
//!
//! A publish-subscribe system similar to Phoenix.PubSub.
//! Uses a local Registry for efficient local dispatch and
//! `pg` for distributed process group membership.
//!
//! # Architecture
//!
//! - **Registry**: Handles local subscriptions with fast dispatch
//! - **pg**: Handles cross-node group membership for discovering remote subscribers
//!
//! When broadcasting:
//! 1. Use Registry to dispatch to local subscribers (efficient, no serialization)
//! 2. Use pg to find and send to remote subscribers
//!
//! # Example
//!
//! ```ignore
//! use crate::pubsub::PubSub;
//!
//! // Subscribe the current process to a topic
//! PubSub::subscribe("room:lobby");
//!
//! // Or subscribe a specific PID
//! PubSub::subscribe_pid("room:lobby", some_pid);
//!
//! // Broadcast to all subscribers
//! PubSub::broadcast("room:lobby", &MyMessage { ... });
//!
//! // Unsubscribe
//! PubSub::unsubscribe("room:lobby");
//! ```

use starlang::dist::pg;
use starlang::registry::Registry;
use starlang_core::Pid;
use serde::Serialize;
use std::sync::{Arc, OnceLock};

/// Global local registry for pub/sub subscriptions.
static LOCAL_REGISTRY: OnceLock<Arc<Registry<String, ()>>> = OnceLock::new();

/// Get or initialize the local registry.
fn local_registry() -> &'static Arc<Registry<String, ()>> {
    LOCAL_REGISTRY.get_or_init(|| Registry::new_duplicate("pubsub"))
}

/// PubSub client API.
///
/// Uses Registry for local subscriptions and pg for distribution.
/// This provides efficient local dispatch while maintaining
/// cross-node pub/sub capabilities.
pub struct PubSub;

/// Allow dead code for API completeness - these functions may be used by consumers.
#[allow(dead_code)]
impl PubSub {
    /// Subscribe the current process to a topic.
    pub fn subscribe(topic: &str) {
        Self::subscribe_pid(topic, starlang::current_pid());
    }

    /// Subscribe a specific PID to a topic.
    ///
    /// This registers the PID in pg for distribution, and in the local
    /// Registry only if the PID is local (for efficient local dispatch).
    pub fn subscribe_pid(topic: &str, pid: Pid) {
        // Only register in local Registry if the PID is actually local
        // This prevents double-delivery when broadcasting
        if pid.is_local() {
            let topic_key = topic.to_string();
            let _ = local_registry().register(topic_key, pid, ());
        }

        // Join pg group for cross-node discovery
        pg::join(Self::topic_to_group(topic), pid);
    }

    /// Unsubscribe the current process from a topic.
    pub fn unsubscribe(topic: &str) {
        Self::unsubscribe_pid(topic, starlang::current_pid());
    }

    /// Unsubscribe a specific PID from a topic.
    pub fn unsubscribe_pid(topic: &str, pid: Pid) {
        // Remove from local registry only if local
        if pid.is_local() {
            let topic_key = topic.to_string();
            local_registry().unregister(&topic_key, pid);
        }

        // Leave pg group
        pg::leave(&Self::topic_to_group(topic), pid);
    }

    /// Unsubscribe a PID from all topics.
    pub fn unsubscribe_all(pid: Pid) {
        // Remove from local registry
        local_registry().unregister_all(pid);

        // Leave all pg groups
        pg::leave_all(pid);
    }

    /// Broadcast a serializable message to all subscribers of a topic.
    ///
    /// This sends to all subscribers across all nodes.
    pub fn broadcast<M: Serialize>(topic: &str, message: &M) {
        Self::do_broadcast(topic, message, None);
    }

    /// Broadcast a serializable message to all subscribers except the specified PID.
    ///
    /// This is useful when the sender is also subscribed to the topic
    /// but shouldn't receive their own message.
    pub fn broadcast_from<M: Serialize>(topic: &str, message: &M, exclude: Pid) {
        Self::do_broadcast(topic, message, Some(exclude));
    }

    /// Internal: broadcast to all subscribers.
    ///
    /// Uses Registry for local dispatch (efficient) and pg for remote.
    fn do_broadcast<M: Serialize>(topic: &str, message: &M, exclude: Option<Pid>) {
        let topic_key = topic.to_string();

        // Serialize once for all sends
        let payload = match postcard::to_allocvec(message) {
            Ok(p) => p,
            Err(_) => return,
        };

        // Use Registry dispatch for local subscribers (efficient)
        local_registry().dispatch(&topic_key, |entries| {
            for (pid, _) in entries {
                if Some(*pid) != exclude {
                    let _ = starlang::send_raw(*pid, payload.clone());
                }
            }
        });

        // For remote subscribers, we need to check pg for members not in local registry
        // pg::get_members returns all members (local + remote)
        // We only want to send to remote members here since local was handled above
        let group = Self::topic_to_group(topic);
        let all_members = pg::get_members(&group);
        let local_members = pg::get_local_members(&group);

        for pid in all_members {
            // Skip local members (already handled by Registry dispatch)
            if local_members.contains(&pid) {
                continue;
            }
            // Skip excluded PID
            if Some(pid) == exclude {
                continue;
            }
            let _ = starlang::send_raw(pid, payload.clone());
        }
    }

    /// Get the list of subscribers for a topic (local + remote).
    pub fn subscribers(topic: &str) -> Vec<Pid> {
        pg::get_members(&Self::topic_to_group(topic))
    }

    /// Get only local subscribers for a topic.
    pub fn local_subscribers(topic: &str) -> Vec<Pid> {
        local_registry()
            .lookup(&topic.to_string())
            .into_iter()
            .map(|(pid, _)| pid)
            .collect()
    }

    /// Get all topics.
    pub fn topics() -> Vec<String> {
        local_registry().all_keys()
    }

    /// Get the count of subscribers for a topic (local only).
    pub fn count(topic: &str) -> usize {
        local_registry().count(&topic.to_string())
    }

    /// Convert a topic name to a pg group name.
    /// We prefix with "pubsub:" to namespace our groups.
    fn topic_to_group(topic: &str) -> String {
        format!("pubsub:{}", topic)
    }
}
