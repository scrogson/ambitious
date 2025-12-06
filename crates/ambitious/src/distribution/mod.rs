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
