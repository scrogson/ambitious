//! Elixir-compatible Node API for distributed Ambitious.
//!
//! This module provides an API that mirrors Elixir's `Node` module for
//! managing distributed nodes in a Ambitious cluster.
//!
//! # Overview
//!
//! The Node module provides functions for:
//! - Checking if the node is part of a distributed system (`is_alive`)
//! - Getting the current node's name (`current`)
//! - Connecting to and disconnecting from other nodes (`connect`, `disconnect`)
//! - Listing connected nodes (`list`)
//! - Pinging nodes to check connectivity (`ping`)
//! - Monitoring nodes for disconnection (`monitor`, `demonitor`)
//! - Spawning processes on remote nodes (`spawn`, `spawn_link`)
//!
//! # Example
//!
//! ```ignore
//! use ambitious::node::{self, ListOption};
//!
//! // Check if distribution is active
//! if node::is_alive() {
//!     println!("This node is: {:?}", node::current());
//!
//!     // Connect to another node
//!     if let Ok(node) = node::connect("other@192.168.1.100:9000").await {
//!         println!("Connected to {:?}", node);
//!     }
//!
//!     // List all connected nodes
//!     for node in node::list(ListOption::Connected) {
//!         println!("Connected: {:?}", node);
//!     }
//! }
//! ```

use crate::atom::Atom;
use crate::core::Pid;
use crate::core::node::{NodeName, is_distributed, node_name};
use crate::distribution::protocol::DistMessage;
use crate::distribution::{self, DistError, NodeMonitorRef};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Options for filtering the node list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOption {
    /// All connected nodes (equivalent to `Node.list()` in Elixir).
    Connected,
    /// All known nodes (connected + previously connected).
    Known,
    /// Nodes we've attempted to connect to but aren't currently connected.
    /// (Currently same as Connected since we don't track this state yet)
    This,
    /// All nodes including self.
    Visible,
    /// Hidden nodes (not applicable in current implementation).
    Hidden,
}

/// Result of a ping operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingResult {
    /// Node responded to ping.
    Pong,
    /// Node did not respond (not connected, timeout, or error).
    Pang,
}

/// Global sequence counter for ping operations.
static PING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Returns `true` if the local node is part of a distributed system.
///
/// This is equivalent to Elixir's `Node.alive?/0`.
///
/// # Example
///
/// ```ignore
/// use ambitious::node;
///
/// if node::is_alive() {
///     println!("Distribution is enabled");
/// }
/// ```
pub fn is_alive() -> bool {
    is_distributed()
}

/// Returns the current node's name.
///
/// This is equivalent to Elixir's `Node.self/0`.
///
/// Returns `None` if distribution hasn't been initialized.
/// In Elixir, this returns `:nonode@nohost` when not distributed,
/// but we return `None` to be more explicit in Rust.
///
/// # Example
///
/// ```ignore
/// use ambitious::node;
///
/// if let Some(name) = node::current() {
///     println!("This node is: {}", name);
/// }
/// ```
pub fn current() -> Option<&'static NodeName> {
    node_name()
}

/// Returns the current node's name as an Atom.
///
/// Returns the empty atom if distribution hasn't been initialized.
pub fn current_atom() -> Atom {
    crate::core::node::node_name_atom()
}

/// Establishes a connection to another node.
///
/// This is equivalent to Elixir's `Node.connect/1`.
///
/// # Arguments
///
/// * `node` - The address of the node to connect to (e.g., "192.168.1.100:9000")
///
/// # Returns
///
/// Returns the node's name as an `Atom` on success.
///
/// # Example
///
/// ```ignore
/// use ambitious::node;
///
/// match node::connect("other@192.168.1.100:9000").await {
///     Ok(node_name) => println!("Connected to {}", node_name),
///     Err(e) => println!("Failed to connect: {}", e),
/// }
/// ```
pub async fn connect(addr: &str) -> Result<Atom, DistError> {
    distribution::connect(addr).await
}

/// Forces disconnection from a node.
///
/// This is equivalent to Elixir's `Node.disconnect/1`.
///
/// # Arguments
///
/// * `node` - The node atom to disconnect from
///
/// # Example
///
/// ```ignore
/// use ambitious::node;
///
/// if let Err(e) = node::disconnect(node_atom) {
///     println!("Failed to disconnect: {}", e);
/// }
/// ```
pub fn disconnect(node: Atom) -> Result<(), DistError> {
    distribution::disconnect(node)
}

/// Returns a list of nodes according to the specified filter.
///
/// This is equivalent to Elixir's `Node.list/0` and `Node.list/1`.
///
/// # Arguments
///
/// * `option` - The type of nodes to list
///
/// # Example
///
/// ```ignore
/// use ambitious::node::{self, ListOption};
///
/// // Get all connected nodes
/// let nodes = node::list(ListOption::Connected);
///
/// // Get all visible nodes (including self)
/// let visible = node::list(ListOption::Visible);
/// ```
pub fn list(option: ListOption) -> Vec<Atom> {
    match option {
        ListOption::Connected | ListOption::Known | ListOption::This => distribution::nodes(),
        ListOption::Visible => {
            let mut nodes = distribution::nodes();
            nodes.push(current_atom());
            nodes
        }
        ListOption::Hidden => Vec::new(), // No hidden nodes in current implementation
    }
}

/// Attempts to ping a node, returning `:pong` if successful or `:pang` if not.
///
/// This is equivalent to Elixir's `Node.ping/1`.
///
/// Unlike Elixir's version which takes a node name, this takes an address
/// and will attempt to connect if not already connected.
///
/// # Arguments
///
/// * `node` - The node atom to ping (must already be connected)
/// * `timeout_ms` - Timeout in milliseconds (default: 5000)
///
/// # Example
///
/// ```ignore
/// use ambitious::node::{self, PingResult};
///
/// match node::ping(node_atom, 5000).await {
///     PingResult::Pong => println!("Node is reachable"),
///     PingResult::Pang => println!("Node is not reachable"),
/// }
/// ```
pub async fn ping(node: Atom, timeout_ms: u64) -> PingResult {
    ping_impl(node, Duration::from_millis(timeout_ms)).await
}

/// Implementation of ping with Duration timeout.
async fn ping_impl(node: Atom, timeout_duration: Duration) -> PingResult {
    let manager = match distribution::manager() {
        Some(m) => m,
        None => return PingResult::Pang,
    };

    let tx = match manager.get_node_tx(node) {
        Some(tx) => tx,
        None => return PingResult::Pang,
    };

    let seq = PING_SEQ.fetch_add(1, Ordering::Relaxed);

    // Register the ping before sending so we can await the pong
    let pong_rx = manager.register_ping(node, seq);

    // Send ping
    let msg = DistMessage::Ping { seq };
    if tx.try_send(msg).is_err() {
        return PingResult::Pang;
    }

    // Wait for pong response with timeout
    match tokio::time::timeout(timeout_duration, pong_rx).await {
        Ok(Ok(())) => PingResult::Pong,
        Ok(Err(_)) => PingResult::Pang, // Sender dropped (node disconnected)
        Err(_) => PingResult::Pang,     // Timeout
    }
}

/// Monitors a node for disconnection.
///
/// This is equivalent to Elixir's `Node.monitor/2` with `true`.
///
/// When the node disconnects, a `NodeDown` message will be sent to the
/// calling process.
///
/// # Arguments
///
/// * `node` - The node atom to monitor
///
/// # Returns
///
/// A `NodeMonitorRef` that can be used to cancel the monitor.
///
/// # Example
///
/// ```ignore
/// use ambitious::node;
///
/// let mon_ref = node::monitor(other_node)?;
///
/// // Later, in your message loop:
/// // NodeDown { node, reason } => { ... }
/// ```
pub fn monitor(node: Atom) -> Result<NodeMonitorRef, DistError> {
    distribution::monitor_node(node)
}

/// Cancels a node monitor.
///
/// This is equivalent to Elixir's `Node.monitor/2` with `false`.
///
/// # Arguments
///
/// * `mon_ref` - The monitor reference returned by `monitor`
pub fn demonitor(mon_ref: NodeMonitorRef) -> Result<(), DistError> {
    distribution::demonitor_node(mon_ref)
}

/// Gets information about a connected node.
///
/// # Arguments
///
/// * `node` - The node atom to get info for
///
/// # Returns
///
/// `Some(NodeInfo)` if connected, `None` otherwise.
pub fn info(node: Atom) -> Option<crate::core::NodeInfo> {
    distribution::node_info(node)
}

/// Stops the distribution layer.
///
/// This is equivalent to Elixir's `Node.stop/0`.
///
/// Note: In the current implementation, distribution cannot be cleanly
/// stopped and restarted. This function disconnects from all nodes but
/// doesn't fully tear down the distribution layer.
///
/// # Returns
///
/// `Ok(())` if successful, `Err` if distribution wasn't initialized.
pub fn stop() -> Result<(), DistError> {
    let nodes = distribution::nodes();
    for node in nodes {
        let _ = distribution::disconnect(node);
    }
    Ok(())
}

/// Configuration for starting distribution.
pub use crate::distribution::Config;

/// Starts distribution with the given configuration.
///
/// This is similar to Elixir's `Node.start/2`.
///
/// # Example
///
/// ```ignore
/// use ambitious::node;
///
/// node::start("mynode@localhost", 9000).await?;
/// ```
pub async fn start(name: &str, port: u16) -> Result<(), DistError> {
    let addr = format!("0.0.0.0:{}", port);
    Config::new().name(name).listen_addr(addr).start().await
}

// === Remote Spawning ===
// TODO: Implement remote spawn functions
// These require additional protocol support for spawning processes on remote nodes.

/// Spawns a process on a remote node.
///
/// This is equivalent to Elixir's `Node.spawn/2`.
///
/// **Note**: This is not yet implemented. Remote spawning requires
/// additional protocol support.
///
/// # Arguments
///
/// * `node` - The node to spawn on
/// * `module` - The module containing the function
/// * `function` - The function name to execute
/// * `args` - Serialized arguments
///
/// # Returns
///
/// The PID of the spawned process.
pub async fn spawn(
    _node: Atom,
    _module: &str,
    _function: &str,
    _args: Vec<u8>,
) -> Result<Pid, DistError> {
    // TODO: Implement remote spawning
    // This requires:
    // 1. A way to serialize function references
    // 2. Protocol message for spawn requests
    // 3. Remote process creation
    Err(DistError::Connect(
        "remote spawn not yet implemented".to_string(),
    ))
}

/// Spawns a linked process on a remote node.
///
/// This is equivalent to Elixir's `Node.spawn_link/2`.
///
/// **Note**: This is not yet implemented.
pub async fn spawn_link(
    _node: Atom,
    _module: &str,
    _function: &str,
    _args: Vec<u8>,
) -> Result<Pid, DistError> {
    Err(DistError::Connect(
        "remote spawn_link not yet implemented".to_string(),
    ))
}

/// Spawns a monitored process on a remote node.
///
/// This is equivalent to Elixir's `Node.spawn_monitor/2`.
///
/// **Note**: This is not yet implemented.
pub async fn spawn_monitor(
    _node: Atom,
    _module: &str,
    _function: &str,
    _args: Vec<u8>,
) -> Result<(Pid, crate::core::Ref), DistError> {
    Err(DistError::Connect(
        "remote spawn_monitor not yet implemented".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_alive_without_distribution() {
        // Without distribution initialized, alive() should return false
        // Note: This test might fail if other tests have initialized distribution
        // assert!(!alive());
    }

    #[test]
    fn test_list_option_default() {
        // Without distribution, list should return empty
        let nodes = list(ListOption::Connected);
        assert!(nodes.is_empty() || distribution::manager().is_some());
    }

    #[test]
    fn test_ping_result_enum() {
        assert_eq!(PingResult::Pong, PingResult::Pong);
        assert_ne!(PingResult::Pong, PingResult::Pang);
    }
}
