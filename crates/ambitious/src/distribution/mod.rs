//! Distribution layer for Ambitious.
//!
//! This module provides the ability to connect Ambitious nodes together,
//! enabling transparent message passing between processes on different nodes.
//!
//! # Architecture
//!
//! The distribution layer is organized into several components:
//!
//! - **Protocol**: Wire format for inter-node messages
//! - **Transport**: QUIC-based secure connections
//! - **Manager**: Connection lifecycle and node registry
//! - **Monitor**: Node-level monitoring for fault detection
//! - **Discovery**: Pluggable node discovery (trait-based)
//!
//! # Node Identification
//!
//! Nodes are identified by their name (as an `Atom`), not by numeric IDs.
//! This makes PIDs globally unambiguous - a PID like `<node2@localhost.5.0>`
//! means the same thing on any node.
//!
//! # Quick Start
//!
//! ```ignore
//! use ambitious::dist;
//!
//! // Start listening for incoming connections
//! dist::listen("0.0.0.0:9000").await?;
//!
//! // Connect to another node
//! let node = dist::connect("192.168.1.100:9000").await?;
//!
//! // Send to a remote process (transparent - just use the PID)
//! ambitious::send_raw(remote_pid, message);
//! ```

mod discovery;
pub mod erlang;
pub mod global;
mod manager;
mod monitor;
mod node;
pub mod pg;
mod process_monitor;
pub(crate) mod protocol;
mod tcp_transport;
mod traits;
mod transport;

#[cfg(test)]
mod tests;

pub use discovery::NodeDiscovery;
pub use manager::{NodeType, connect, disconnect, node_info, nodes};
pub use monitor::{NodeDown, NodeDownReason, NodeMonitorRef, demonitor_node, monitor_node};
pub use node::{Config, init_distribution};
pub use process_monitor::{demonitor_process, link_process, monitor_process, unlink_process};
pub use protocol::DistError;
pub use tcp_transport::{TcpConnection, TcpTransport};
pub use traits::{PostcardProtocol, Protocol, Transport, TransportConnection, TransportType};

// Re-export Erlang types when feature is enabled
#[cfg(feature = "erlang-dist")]
pub use erlang::{
    Connection as ErlangConnectionRaw, ConnectionConfig as ErlangConnectionConfig, ControlMessage,
    DistributionFlags, ErlangConfig, ErlangConnection, ErlangMessage, ErlangPid, ErlangRef,
};

use std::sync::OnceLock;

/// Global distribution manager.
static DIST_MANAGER: OnceLock<manager::DistributionManager> = OnceLock::new();

/// Get the global distribution manager.
///
/// Returns `None` if distribution hasn't been initialized.
pub(crate) fn manager() -> Option<&'static manager::DistributionManager> {
    DIST_MANAGER.get()
}

/// Start listening for incoming distribution connections.
///
/// This is a convenience function that initializes distribution with defaults
/// and starts listening on the specified address.
///
/// # Example
///
/// ```ignore
/// ambitious::dist::listen("0.0.0.0:9000").await?;
/// ```
pub async fn listen(addr: &str) -> Result<(), DistError> {
    let config = Config::new().listen_addr(addr);
    config.start().await
}

/// Check if a node is a BEAM/Erlang node.
///
/// Returns `true` if the node is connected via Erlang Distribution Protocol,
/// `false` if it's a native Ambitious node or not connected.
#[cfg(feature = "erlang-dist")]
pub fn is_beam_node(node: crate::core::Atom) -> bool {
    DIST_MANAGER
        .get()
        .and_then(|m| m.node_type(node))
        .map(|t| matches!(t, NodeType::Erlang))
        .unwrap_or(false)
}

/// Check if a node is a BEAM/Erlang node.
///
/// Always returns `false` when the `erlang-dist` feature is not enabled.
#[cfg(not(feature = "erlang-dist"))]
pub fn is_beam_node(_node: crate::core::Atom) -> bool {
    false
}

/// Send a message to a BEAM node.
///
/// The payload should already be encoded as ETF bytes.
#[cfg(feature = "erlang-dist")]
pub fn send_to_beam(pid: crate::core::Pid, etf_bytes: Vec<u8>) -> Result<(), DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    manager.send_to_erlang(pid, etf_bytes)
}

/// Connect to an Erlang/BEAM node.
///
/// This establishes a connection using the Erlang Distribution Protocol,
/// enabling transparent message exchange with Erlang, Elixir, and other
/// BEAM-based systems.
///
/// # Arguments
///
/// * `remote_node` - The remote node name (e.g., "elixir@localhost")
/// * `cookie` - The shared secret cookie for authentication
///
/// # Example
///
/// ```ignore
/// use ambitious::dist;
///
/// // Connect to an Elixir node
/// dist::connect_erlang("my_app@localhost", "secret_cookie").await?;
///
/// // Now you can send messages to BEAM processes transparently
/// ambitious::send(beam_pid, MyMessage { data: 42 });
/// ```
#[cfg(feature = "erlang-dist")]
pub async fn connect_erlang(
    remote_node: &str,
    cookie: &str,
) -> Result<crate::core::Atom, DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    manager.connect_erlang(remote_node, cookie).await
}
