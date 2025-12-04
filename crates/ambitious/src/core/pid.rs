//! Process identifier type.
//!
//! A [`Pid`] uniquely identifies a process within the Ambitious runtime. It consists of
//! three components, matching Erlang's PID format `<node.id.creation>`:
//!
//! - **node**: Which node the process belongs to (as an Atom for global uniqueness)
//! - **id**: The unique process identifier within that node
//! - **creation**: Distinguishes PIDs across node restarts
//!
//! The node is stored as an [`Atom`] (interned string) so that PIDs are globally
//! unambiguous - `<node1@localhost.5.0>` means the same thing on any node.
//!
//! The creation number prevents stale PIDs from accidentally matching new processes
//! after a node restart.

use super::node::node_name_atom;
use crate::atom::Atom;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Global counter for generating unique process IDs.
static PID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Current node creation number. Incremented on each node restart.
static CREATION: AtomicU32 = AtomicU32::new(0);

/// A process identifier.
///
/// Every process in Ambitious has a unique `Pid` that can be used to send messages,
/// establish links, create monitors, and query process status.
///
/// The node is stored as an [`Atom`] (interned string) for global uniqueness.
/// Display format matches Erlang's `<node.id.creation>` convention, showing
/// `<0.id.creation>` for local PIDs and `<N.id.creation>` for remote ones.
///
/// # Examples
///
/// ```
/// use ambitious::core::Pid;
///
/// let pid = Pid::new();
/// println!("Process: {}", pid);  // e.g., "<0.42.0>"
///
/// // Pids can be compared
/// let pid2 = Pid::new();
/// assert_ne!(pid, pid2);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pid {
    /// Node identifier as an atom (e.g., "node1@localhost").
    node: Atom,
    /// Unique process identifier within the node.
    id: u64,
    /// Creation number - distinguishes PIDs across node restarts.
    creation: u32,
}

impl Pid {
    /// Creates a new unique process identifier on the local node.
    ///
    /// Each call to `new()` returns a globally unique `Pid`.
    /// The node is set to the current node's name atom.
    ///
    /// # Examples
    ///
    /// ```
    /// use ambitious::core::Pid;
    ///
    /// let pid1 = Pid::new();
    /// let pid2 = Pid::new();
    /// assert_ne!(pid1, pid2);
    /// ```
    pub fn new() -> Self {
        Self {
            node: node_name_atom(),
            id: PID_COUNTER.fetch_add(1, Ordering::Relaxed),
            creation: CREATION.load(Ordering::Relaxed),
        }
    }

    /// Creates a `Pid` with a specific node atom, id, and creation values.
    ///
    /// This is primarily used for deserialization and testing. In normal usage,
    /// prefer [`Pid::new()`].
    pub fn from_parts_atom(node: Atom, id: u64, creation: u32) -> Self {
        Self { node, id, creation }
    }

    /// Creates a remote `Pid` for a process on another node.
    ///
    /// This is used when receiving PIDs from remote nodes during distribution.
    ///
    /// # Examples
    ///
    /// ```
    /// use ambitious::core::Pid;
    ///
    /// let remote_pid = Pid::remote("node2@localhost", 100, 5);
    /// assert!(!remote_pid.is_local());
    /// ```
    pub fn remote(node_name: &str, id: u64, creation: u32) -> Self {
        Self {
            node: Atom::new(node_name),
            id,
            creation,
        }
    }

    /// Returns the node atom.
    #[inline]
    pub fn node(&self) -> Atom {
        self.node
    }

    /// Returns the node name as a string.
    #[inline]
    pub fn node_name(&self) -> String {
        self.node.as_str()
    }

    /// Returns the process identifier within the node.
    #[inline]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Returns the creation number.
    ///
    /// The creation number distinguishes PIDs across node restarts.
    #[inline]
    pub const fn creation(&self) -> u32 {
        self.creation
    }

    /// Returns `true` if this is a local process.
    ///
    /// A PID is local if its node matches the current node's name.
    #[inline]
    pub fn is_local(&self) -> bool {
        self.node == node_name_atom()
    }
}

/// Increment the creation counter.
///
/// This should be called when the node restarts to invalidate old PIDs.
/// Returns the new creation value.
pub fn increment_creation() -> u32 {
    CREATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// Get the current creation value.
pub fn current_creation() -> u32 {
    CREATION.load(Ordering::Relaxed)
}

impl Default for Pid {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_local() {
            write!(f, "Pid<0.{}.{}>", self.id, self.creation)
        } else {
            // Show the node name for remote PIDs
            write!(f, "Pid<{}.{}.{}>", self.node, self.id, self.creation)
        }
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_local() {
            write!(f, "<0.{}.{}>", self.id, self.creation)
        } else {
            // Show the node name for remote PIDs
            write!(f, "<{}.{}.{}>", self.node, self.id, self.creation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom;

    #[test]
    fn test_pid_uniqueness() {
        let pid1 = Pid::new();
        let pid2 = Pid::new();
        assert_ne!(pid1, pid2);
    }

    #[test]
    fn test_pid_local() {
        let pid = Pid::new();
        assert!(pid.is_local());
    }

    #[test]
    fn test_pid_remote() {
        let pid = Pid::remote("node2@localhost", 100, 2);
        assert_eq!(pid.node_name(), "node2@localhost");
        assert_eq!(pid.id(), 100);
        assert_eq!(pid.creation(), 2);
        assert!(!pid.is_local());
    }

    #[test]
    fn test_pid_from_parts_atom() {
        let node = atom!("test@host");
        let pid = Pid::from_parts_atom(node, 42, 1);
        assert_eq!(pid.node(), node);
        assert_eq!(pid.id(), 42);
        assert_eq!(pid.creation(), 1);
    }

    #[test]
    fn test_pid_display_local() {
        let pid = Pid::new();
        // Local PIDs show as <0.id.creation>
        let display = format!("{}", pid);
        assert!(
            display.starts_with("<0."),
            "expected local display format, got: {}",
            display
        );
        // Format is <0.id.creation> - verify structure
        let parts: Vec<&str> = display
            .trim_matches(|c| c == '<' || c == '>')
            .split('.')
            .collect();
        assert_eq!(parts.len(), 3, "expected 3 parts in PID display");
        assert_eq!(parts[0], "0", "expected node 0 for local PID");
    }

    #[test]
    fn test_pid_display_remote() {
        let pid = Pid::remote("node2@host", 42, 0);
        assert_eq!(format!("{}", pid), "<node2@host.42.0>");
        assert_eq!(format!("{:?}", pid), "Pid<node2@host.42.0>");
    }

    #[test]
    fn test_pid_serialization() {
        let pid = Pid::remote("node1@localhost", 123, 5);
        let bytes = postcard::to_allocvec(&pid).unwrap();
        let decoded: Pid = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(pid, decoded);
        assert_eq!(decoded.node_name(), "node1@localhost");
    }

    #[test]
    fn test_pid_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        let pid1 = Pid::new();
        let pid2 = Pid::new();

        set.insert(pid1);
        set.insert(pid2);
        set.insert(pid1); // duplicate

        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_creation_distinguishes_pids() {
        // Same node and id but different creation should be different PIDs
        let node = atom!("test@host");
        let pid1 = Pid::from_parts_atom(node, 42, 0);
        let pid2 = Pid::from_parts_atom(node, 42, 1);
        assert_ne!(pid1, pid2);
    }

    #[test]
    fn test_increment_creation() {
        let before = current_creation();
        let new_creation = increment_creation();
        assert_eq!(new_creation, before + 1);
        assert_eq!(current_creation(), new_creation);
    }
}
