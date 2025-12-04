//! Node identity types for distributed Starlang.
//!
//! These types identify nodes in a Starlang cluster:
//!
//! - [`NodeId`] - Numeric identifier for a node (used for display only)
//! - [`NodeName`] - Human-readable name like "node1@localhost"
//! - [`NodeInfo`] - Complete node information including connection details
//!
//! Node identity is stored as an [`Atom`] for efficient comparison and
//! globally unambiguous PID addressing.

use crate::atom::Atom;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::SocketAddr;
use std::sync::OnceLock;

/// Local node identifier constant (for display purposes).
pub const LOCAL_NODE_ID: u32 = 0;

/// The atom representing the local/uninitialized node.
/// This is used before distribution is initialized.
static LOCAL_NODE_ATOM: OnceLock<Atom> = OnceLock::new();

/// Global storage for this node's identity.
static THIS_NODE: OnceLock<NodeIdentity> = OnceLock::new();

/// Get the atom representing "no node" / local node before distribution init.
fn local_node_atom() -> Atom {
    *LOCAL_NODE_ATOM.get_or_init(|| Atom::new(""))
}

/// A node identifier.
///
/// Each node in a Starlang cluster has a unique numeric ID. The local node
/// always has ID 0. Remote nodes are assigned IDs starting from 1 when
/// connections are established.
///
/// # Examples
///
/// ```
/// use starlang::core::NodeId;
///
/// let local = NodeId::local();
/// assert!(local.is_local());
///
/// let remote = NodeId::new(1);
/// assert!(!remote.is_local());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(u32);

impl NodeId {
    /// Creates a new `NodeId` with the given value.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the local node ID (always 0).
    #[inline]
    pub const fn local() -> Self {
        Self(LOCAL_NODE_ID)
    }

    /// Returns the raw u32 value.
    #[inline]
    pub const fn as_u32(&self) -> u32 {
        self.0
    }

    /// Returns `true` if this is the local node.
    #[inline]
    pub const fn is_local(&self) -> bool {
        self.0 == LOCAL_NODE_ID
    }
}

impl From<u32> for NodeId {
    fn from(id: u32) -> Self {
        Self(id)
    }
}

impl From<NodeId> for u32 {
    fn from(id: NodeId) -> Self {
        id.0
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A human-readable node name.
///
/// Node names follow the format `name@host`, similar to Erlang node names.
/// For example: `"node1@localhost"` or `"chat-server@192.168.1.100"`.
///
/// # Examples
///
/// ```
/// use starlang::core::NodeName;
///
/// let name = NodeName::new("node1@localhost");
/// assert_eq!(name.short_name(), "node1");
/// assert_eq!(name.host(), "localhost");
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeName(String);

impl NodeName {
    /// Creates a new node name.
    ///
    /// The name should be in the format `name@host`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the full node name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the short name (before the @).
    ///
    /// Returns the full name if no @ is present.
    pub fn short_name(&self) -> &str {
        self.0.split('@').next().unwrap_or(&self.0)
    }

    /// Returns the host part (after the @).
    ///
    /// Returns an empty string if no @ is present.
    pub fn host(&self) -> &str {
        self.0.split('@').nth(1).unwrap_or("")
    }
}

impl fmt::Debug for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeName({:?})", self.0)
    }
}

impl fmt::Display for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for NodeName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for NodeName {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Complete information about a node.
///
/// This includes the node's name, numeric ID, network address,
/// and creation number (for distinguishing node restarts).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    /// The node's human-readable name.
    pub name: NodeName,
    /// The node's numeric ID.
    pub id: NodeId,
    /// The node's network address for distribution.
    pub addr: Option<SocketAddr>,
    /// Creation number - incremented on each node restart.
    pub creation: u32,
}

impl NodeInfo {
    /// Creates new node info.
    pub fn new(
        name: impl Into<NodeName>,
        id: NodeId,
        addr: Option<SocketAddr>,
        creation: u32,
    ) -> Self {
        Self {
            name: name.into(),
            id,
            addr,
            creation,
        }
    }
}

/// The identity of this node.
///
/// Set once at startup and never changes.
#[derive(Clone, Debug)]
pub struct NodeIdentity {
    /// This node's name as a string.
    pub name: NodeName,
    /// This node's name as an atom (for efficient PID comparison).
    pub name_atom: Atom,
    /// This node's creation number.
    pub creation: u32,
}

/// Initialize this node's identity.
///
/// This should be called once at startup. Returns an error if already initialized.
///
/// # Examples
///
/// ```
/// use starlang::core::node::{init_node, NodeName};
///
/// // Only call once at startup
/// // init_node(NodeName::new("node1@localhost"), 0);
/// ```
pub fn init_node(name: NodeName, creation: u32) -> Result<(), NodeIdentity> {
    let name_atom = Atom::new(name.as_str());
    THIS_NODE.set(NodeIdentity {
        name,
        name_atom,
        creation,
    })
}

/// Get this node's name.
///
/// Returns `None` if the node hasn't been initialized with distribution enabled.
pub fn node_name() -> Option<&'static NodeName> {
    THIS_NODE.get().map(|n| &n.name)
}

/// Get this node's name as an Atom.
///
/// Returns the empty atom if distribution hasn't been initialized.
/// This is used for PID node field comparison.
pub fn node_name_atom() -> Atom {
    THIS_NODE
        .get()
        .map(|n| n.name_atom)
        .unwrap_or_else(local_node_atom)
}

/// Get this node's creation number.
///
/// Returns 0 if not initialized.
pub fn node_creation() -> u32 {
    THIS_NODE.get().map(|n| n.creation).unwrap_or(0)
}

/// Returns `true` if distribution has been initialized.
pub fn is_distributed() -> bool {
    THIS_NODE.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id_local() {
        let local = NodeId::local();
        assert!(local.is_local());
        assert_eq!(local.as_u32(), 0);
    }

    #[test]
    fn test_node_id_remote() {
        let remote = NodeId::new(5);
        assert!(!remote.is_local());
        assert_eq!(remote.as_u32(), 5);
    }

    #[test]
    fn test_node_name_parsing() {
        let name = NodeName::new("mynode@example.com");
        assert_eq!(name.short_name(), "mynode");
        assert_eq!(name.host(), "example.com");
    }

    #[test]
    fn test_node_name_no_at() {
        let name = NodeName::new("standalone");
        assert_eq!(name.short_name(), "standalone");
        assert_eq!(name.host(), "");
    }

    #[test]
    fn test_node_id_serialization() {
        let id = NodeId::new(42);
        let bytes = postcard::to_allocvec(&id).unwrap();
        let decoded: NodeId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(id, decoded);
    }
}
