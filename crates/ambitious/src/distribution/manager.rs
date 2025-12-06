//! Distribution connection manager.
//!
//! Manages connections to remote nodes and routes messages.
//! Nodes are identified by their name (as an Atom) for globally unique addressing.

use super::DIST_MANAGER;
use super::monitor::NodeMonitorRegistry;
use super::process_monitor::ProcessMonitorRegistry;
use super::protocol::{DistError, DistMessage};
use super::transport::{QuicConnection, QuicTransport};
use crate::core::{Atom, NodeInfo, NodeName, Pid, Ref};
use dashmap::{DashMap, DashSet};
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

/// How often to send heartbeat pings to connected nodes.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// How long to wait for a pong before considering a node dead.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);

/// Information about a connected node.
struct ConnectedNode {
    /// The node's info.
    info: NodeInfo,
    /// The QUIC connection.
    connection: QuicConnection,
    /// Sender for outgoing messages.
    tx: mpsc::Sender<DistMessage>,
    /// Last time we received a pong (or any message) from this node.
    /// Stored as milliseconds since UNIX epoch for atomic access.
    last_seen_ms: AtomicU64,
}

impl ConnectedNode {
    /// Create a new connected node with current timestamp.
    fn new(info: NodeInfo, connection: QuicConnection, tx: mpsc::Sender<DistMessage>) -> Self {
        Self {
            info,
            connection,
            tx,
            last_seen_ms: AtomicU64::new(current_time_ms()),
        }
    }

    /// Update the last seen timestamp to now.
    fn touch(&self) {
        self.last_seen_ms
            .store(current_time_ms(), Ordering::Relaxed);
    }

    /// Check if this node has timed out (no response within HEARTBEAT_TIMEOUT).
    fn is_timed_out(&self) -> bool {
        let last_seen = self.last_seen_ms.load(Ordering::Relaxed);
        let now = current_time_ms();
        now.saturating_sub(last_seen) > HEARTBEAT_TIMEOUT.as_millis() as u64
    }
}

/// Get current time in milliseconds since UNIX epoch.
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The distribution manager.
///
/// Handles all node connections and message routing.
/// Nodes are identified by their name (as an Atom) rather than numeric IDs.
pub struct DistributionManager {
    /// Our node name.
    node_name: String,
    /// Our node name as an atom.
    node_name_atom: Atom,
    /// Our creation number.
    creation: u32,
    /// Connected nodes by node name atom.
    nodes: DashMap<Atom, ConnectedNode>,
    /// Node name lookup by address.
    addr_to_node: DashMap<SocketAddr, Atom>,
    /// Known node addresses (persists across disconnects for reconnection).
    known_nodes: DashMap<Atom, SocketAddr>,
    /// Nodes currently being reconnected to (prevents concurrent reconnect attempts).
    reconnecting: DashSet<Atom>,
    /// The QUIC transport (if listening).
    transport: RwLock<Option<Arc<QuicTransport>>>,
    /// Node monitor registry.
    monitors: NodeMonitorRegistry,
    /// Process monitor/link registry.
    process_monitors: ProcessMonitorRegistry,
}

impl DistributionManager {
    /// Create a new distribution manager.
    pub fn new(node_name: String, creation: u32) -> Self {
        let node_name_atom = Atom::new(&node_name);
        Self {
            node_name,
            node_name_atom,
            creation,
            nodes: DashMap::new(),
            addr_to_node: DashMap::new(),
            known_nodes: DashMap::new(),
            reconnecting: DashSet::new(),
            transport: RwLock::new(None),
            monitors: NodeMonitorRegistry::new(),
            process_monitors: ProcessMonitorRegistry::new(),
        }
    }

    /// Start listening for incoming connections.
    pub async fn start_listener(
        &self,
        addr: SocketAddr,
        cert_path: Option<impl AsRef<Path>>,
        key_path: Option<impl AsRef<Path>>,
    ) -> Result<(), DistError> {
        let transport = QuicTransport::bind(
            addr,
            self.node_name.clone(),
            self.creation,
            cert_path.as_ref().map(|p| p.as_ref()),
            key_path.as_ref().map(|p| p.as_ref()),
        )
        .await?;

        let transport = Arc::new(transport);
        *self.transport.write() = Some(transport.clone());

        // Spawn accept loop
        let node_name = self.node_name.clone();
        let creation = self.creation;
        tokio::spawn(async move {
            accept_loop(transport, node_name, creation).await;
        });

        // Spawn heartbeat loop to detect dead nodes quickly
        tokio::spawn(async move {
            heartbeat_loop().await;
        });

        Ok(())
    }

    /// Connect to a remote node.
    ///
    /// Returns the remote node's name as an Atom.
    pub async fn connect_to(&self, addr: SocketAddr) -> Result<Atom, DistError> {
        // Check if already connected by address
        if let Some(node_atom) = self.addr_to_node.get(&addr) {
            return Err(DistError::AlreadyConnected(*node_atom));
        }

        // Create a client transport if we don't have one
        let transport = {
            let guard = self.transport.read();
            if let Some(t) = guard.as_ref() {
                t.clone()
            } else {
                drop(guard);
                let t = Arc::new(QuicTransport::client(
                    self.node_name.clone(),
                    self.creation,
                )?);
                *self.transport.write() = Some(t.clone());
                t
            }
        };

        // Connect
        let connection = transport.connect(addr, "localhost").await?;

        // Perform handshake - returns the remote node's name
        let (remote_node_atom, node_info) = self.perform_handshake(&connection, addr).await?;

        // Check if we already have a connection to this node (by different address)
        if self.nodes.contains_key(&remote_node_atom) {
            connection.close("duplicate connection");
            return Err(DistError::AlreadyConnected(remote_node_atom));
        }

        // Create message sender
        let (tx, rx) = mpsc::channel(1024);

        // Store connection
        self.nodes.insert(
            remote_node_atom,
            ConnectedNode::new(node_info, connection, tx),
        );
        self.addr_to_node.insert(addr, remote_node_atom);

        // Remember this node's address for potential reconnection
        self.known_nodes.insert(remote_node_atom, addr);

        // Spawn message sender task
        let node_atom = remote_node_atom;
        let conn_addr = addr;
        tokio::spawn(async move {
            message_sender_loop(rx, node_atom, conn_addr).await;
        });

        // Spawn message receiver task
        let node_atom = remote_node_atom;
        let conn_addr = addr;
        tokio::spawn(async move {
            message_receiver_loop(node_atom, conn_addr).await;
        });

        // Request global registry sync from the new node
        super::global::global_registry().request_sync(remote_node_atom);

        // Request process groups sync from the new node
        super::pg::pg().request_sync(remote_node_atom);

        tracing::info!(%addr, node = %remote_node_atom, "Connected to remote node");
        Ok(remote_node_atom)
    }

    /// Perform the handshake with a remote node.
    async fn perform_handshake(
        &self,
        connection: &QuicConnection,
        addr: SocketAddr,
    ) -> Result<(Atom, NodeInfo), DistError> {
        // Send Hello
        let hello = DistMessage::Hello {
            node_name: self.node_name.clone(),
            creation: self.creation,
        };
        connection.send_message(&hello).await?;

        // Wait for Welcome
        let (_send, mut recv) = connection.accept_stream().await?;
        let welcome = QuicConnection::recv_message(&mut recv).await?;

        match welcome {
            DistMessage::Welcome {
                node_name,
                creation,
            } => {
                let node_atom = Atom::new(&node_name);
                let info = NodeInfo::new(
                    NodeName::new(&node_name),
                    crate::core::NodeId::local(), // NodeId is just for display now
                    Some(addr),
                    creation,
                );
                Ok((node_atom, info))
            }
            _ => Err(DistError::Handshake("expected Welcome message".to_string())),
        }
    }

    /// Disconnect from a node.
    pub fn disconnect_from(&self, node_atom: Atom) -> Result<(), DistError> {
        if let Some((_, node)) = self.nodes.remove(&node_atom) {
            node.connection.close("disconnect requested");
            if let Some(addr) = node.info.addr {
                self.addr_to_node.remove(&addr);
            }

            // Notify node monitors
            self.monitors
                .notify_node_down(node_atom, "disconnect requested".to_string());

            // Clean up process monitors and links for this node
            self.process_monitors.handle_node_down(node_atom);

            // Clean up pg memberships from this node
            super::pg::pg().remove_node_members(node_atom);

            tracing::info!(node = %node_atom, "Disconnected from node");
            Ok(())
        } else {
            Err(DistError::NotConnected(node_atom))
        }
    }

    /// Send a message to a remote process.
    ///
    /// The PID's node field is an Atom identifying the target node.
    ///
    /// If the node is disconnected but we know its address, a reconnection
    /// attempt is triggered in the background. The current send will fail,
    /// but subsequent sends (after reconnection) will succeed.
    pub fn send_to_remote(&self, pid: Pid, payload: Vec<u8>) -> Result<(), DistError> {
        let node_atom = pid.node();

        if let Some(node) = self.nodes.get(&node_atom) {
            let msg = DistMessage::Send {
                to: pid,
                from: crate::runtime::try_current_pid(),
                payload,
            };

            // Non-blocking send
            if node.tx.try_send(msg).is_err() {
                tracing::warn!(?pid, "Message queue full for remote node");
            }
            Ok(())
        } else {
            // Not connected - try to trigger reconnection if we know the address
            self.try_reconnect(node_atom);
            Err(DistError::NotConnected(node_atom))
        }
    }

    /// Trigger a background reconnection attempt to a known node.
    ///
    /// This is called when we try to send to a disconnected node.
    /// If we know the node's address and aren't already reconnecting,
    /// spawn a task to attempt reconnection.
    fn try_reconnect(&self, node_atom: Atom) {
        // Check if we know this node's address
        let addr = match self.known_nodes.get(&node_atom) {
            Some(addr) => *addr,
            None => return, // Unknown node, can't reconnect
        };

        // Check if we're already reconnecting
        if !self.reconnecting.insert(node_atom) {
            // Already reconnecting
            return;
        }

        tracing::debug!(node = %node_atom, %addr, "Attempting reconnection");

        // Spawn reconnection task
        tokio::spawn(async move {
            // Small delay before reconnecting (avoid tight retry loops)
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            let result = if let Some(manager) = DIST_MANAGER.get() {
                manager.connect_to(addr).await
            } else {
                Err(DistError::NotInitialized)
            };

            // Remove from reconnecting set
            if let Some(manager) = DIST_MANAGER.get() {
                manager.reconnecting.remove(&node_atom);
            }

            match result {
                Ok(_) => {
                    tracing::info!(node = %node_atom, "Reconnected successfully");
                }
                Err(e) => {
                    tracing::warn!(node = %node_atom, error = %e, "Reconnection failed");
                }
            }
        });
    }

    /// Get list of connected nodes.
    pub fn connected_nodes(&self) -> Vec<Atom> {
        self.nodes.iter().map(|r| *r.key()).collect()
    }

    /// Get info about a connected node.
    pub fn get_node_info(&self, node_atom: Atom) -> Option<NodeInfo> {
        self.nodes.get(&node_atom).map(|n| n.info.clone())
    }

    /// Get the monitor registry.
    pub fn monitors(&self) -> &NodeMonitorRegistry {
        &self.monitors
    }

    /// Get the process monitor/link registry.
    pub fn process_monitors(&self) -> &ProcessMonitorRegistry {
        &self.process_monitors
    }

    /// Get a node's message sender.
    pub(crate) fn get_node_tx(&self, node_atom: Atom) -> Option<mpsc::Sender<DistMessage>> {
        self.nodes.get(&node_atom).map(|n| n.tx.clone())
    }

    /// Get our node name atom.
    pub fn node_name_atom(&self) -> Atom {
        self.node_name_atom
    }
}

/// Accept loop for incoming connections.
async fn accept_loop(transport: Arc<QuicTransport>, node_name: String, creation: u32) {
    loop {
        if let Some(connection) = transport.accept().await {
            let node_name = node_name.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_incoming_connection(connection, node_name, creation).await {
                    tracing::error!(error = %e, "Failed to handle incoming connection");
                }
            });
        }
    }
}

/// Handle an incoming connection.
async fn handle_incoming_connection(
    connection: QuicConnection,
    our_node_name: String,
    our_creation: u32,
) -> Result<(), DistError> {
    // Wait for Hello
    let (_send, mut recv) = connection.accept_stream().await?;
    let hello = QuicConnection::recv_message(&mut recv).await?;

    let (remote_name, remote_creation) = match hello {
        DistMessage::Hello {
            node_name,
            creation,
        } => (node_name, creation),
        _ => return Err(DistError::Handshake("expected Hello message".to_string())),
    };

    let remote_node_atom = Atom::new(&remote_name);

    // Get the manager
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;

    // If already connected, close the old connection and replace it
    // This handles reconnection after network issues
    if let Some((_, old_node)) = manager.nodes.remove(&remote_node_atom) {
        tracing::info!(node = %remote_node_atom, "Replacing existing connection");
        old_node.connection.close("replaced by new connection");
        if let Some(addr) = old_node.info.addr {
            manager.addr_to_node.remove(&addr);
        }
        // Clean up pg memberships from this node (they'll be re-synced)
        super::pg::pg().remove_node_members(remote_node_atom);
    }

    // Send Welcome with just our node name
    let welcome = DistMessage::Welcome {
        node_name: our_node_name,
        creation: our_creation,
    };
    connection.send_message(&welcome).await?;

    // Store the connection
    let addr = connection.remote_address();
    let (tx, rx) = mpsc::channel(1024);

    let info = NodeInfo::new(
        NodeName::new(&remote_name),
        crate::core::NodeId::local(), // NodeId is just for display now
        Some(addr),
        remote_creation,
    );

    manager
        .nodes
        .insert(remote_node_atom, ConnectedNode::new(info, connection, tx));
    manager.addr_to_node.insert(addr, remote_node_atom);

    // Remember this node's address for potential reconnection
    manager.known_nodes.insert(remote_node_atom, addr);

    // Spawn message handling tasks
    let node_atom = remote_node_atom;
    let conn_addr = addr;
    tokio::spawn(async move {
        message_sender_loop(rx, node_atom, conn_addr).await;
    });

    // Spawn receiver loop
    let node_atom = remote_node_atom;
    let conn_addr = addr;
    tokio::spawn(async move {
        message_receiver_loop(node_atom, conn_addr).await;
    });

    // Send our global registry to the new node
    super::global::global_registry().request_sync(remote_node_atom);

    // Send our process groups to the new node
    super::pg::pg().request_sync(remote_node_atom);

    tracing::info!(
        remote_name = %remote_name,
        "Accepted incoming connection"
    );

    Ok(())
}

/// Loop to send messages to a remote node.
///
/// The `connection_addr` parameter identifies which specific connection this loop
/// is handling. This prevents cleanup races when a connection is replaced.
async fn message_sender_loop(
    mut rx: mpsc::Receiver<DistMessage>,
    node_atom: Atom,
    connection_addr: SocketAddr,
) {
    while let Some(msg) = rx.recv().await {
        let manager = match DIST_MANAGER.get() {
            Some(m) => m,
            None => break,
        };

        if let Some(node) = manager.nodes.get(&node_atom) {
            // Check if this is still our connection
            if node.info.addr != Some(connection_addr) {
                // Connection was replaced, exit without cleanup
                tracing::debug!(
                    node = %node_atom,
                    our_addr = %connection_addr,
                    "Connection replaced, sender loop exiting"
                );
                return;
            }
            if let Err(e) = node.connection.send_message(&msg).await {
                tracing::error!(error = %e, node = %node_atom, "Failed to send message");
                break;
            }
        } else {
            break;
        }
    }

    // Connection closed or error - clean up only if this is still our connection
    if let Some(manager) = DIST_MANAGER.get() {
        let should_cleanup = manager
            .nodes
            .get(&node_atom)
            .is_some_and(|node| node.info.addr == Some(connection_addr));

        if should_cleanup {
            if let Some((_, node)) = manager.nodes.remove(&node_atom) {
                if let Some(addr) = node.info.addr {
                    manager.addr_to_node.remove(&addr);
                }
                manager
                    .monitors
                    .notify_node_down(node_atom, "connection closed".to_string());
                // Clean up process monitors and links for this node
                manager.process_monitors.handle_node_down(node_atom);
                // Clean up pg memberships from this node
                super::pg::pg().remove_node_members(node_atom);
            }
        } else {
            tracing::debug!(
                node = %node_atom,
                our_addr = %connection_addr,
                "Sender: skipping cleanup, connection was replaced"
            );
        }
    }
}

/// Loop to receive messages from a remote node.
///
/// The `connection_addr` parameter identifies which specific connection this loop
/// is handling. This prevents a race condition where an old receiver loop might
/// accidentally clean up a replacement connection.
async fn message_receiver_loop(node_atom: Atom, connection_addr: SocketAddr) {
    while let Some(manager) = DIST_MANAGER.get() {
        // Clone the connection so we don't hold the DashMap lock while awaiting
        let connection = match manager.nodes.get(&node_atom) {
            Some(node) => {
                // Check if this is still our connection (not a replacement)
                if node.info.addr != Some(connection_addr) {
                    // Connection was replaced, exit without cleanup
                    tracing::debug!(
                        node = %node_atom,
                        our_addr = %connection_addr,
                        "Connection replaced, receiver loop exiting"
                    );
                    return;
                }
                // Clone the connection before dropping the lock
                node.connection.clone()
            }
            None => break,
        };

        // Now we can await without holding the DashMap lock
        let mut recv = match connection.accept_stream().await {
            Ok((_, recv)) => recv,
            Err(_) => break,
        };
        match QuicConnection::recv_message(&mut recv).await {
            Ok(msg) => {
                handle_incoming_message(node_atom, msg).await;
            }
            Err(DistError::ConnectionClosed) => break,
            Err(e) => {
                tracing::error!(error = %e, node = %node_atom, "Error receiving message");
                break;
            }
        }
    }

    // Clean up - but only if this is still our connection
    if let Some(manager) = DIST_MANAGER.get() {
        // Check if the current connection is still ours before removing
        let should_cleanup = manager
            .nodes
            .get(&node_atom)
            .is_some_and(|node| node.info.addr == Some(connection_addr));

        if should_cleanup {
            if let Some((_, node)) = manager.nodes.remove(&node_atom) {
                if let Some(addr) = node.info.addr {
                    manager.addr_to_node.remove(&addr);
                }
                manager
                    .monitors
                    .notify_node_down(node_atom, "connection closed".to_string());
                // Clean up process monitors and links for this node
                manager.process_monitors.handle_node_down(node_atom);
                // Clean up pg memberships from this node
                super::pg::pg().remove_node_members(node_atom);
            }
        } else {
            tracing::debug!(
                node = %node_atom,
                our_addr = %connection_addr,
                "Skipping cleanup, connection was replaced"
            );
        }
    }
}

/// Handle an incoming message from a remote node.
async fn handle_incoming_message(from_node: Atom, msg: DistMessage) {
    match msg {
        DistMessage::Send {
            to,
            from: _,
            payload,
        } => {
            // The PID in `to` now contains an Atom for the node.
            // If it matches our node name, deliver locally.
            if to.is_local() {
                // Deliver to local process
                if let Some(handle) = crate::process::global::try_handle() {
                    let _ = handle.registry().send_raw(to, payload);
                }
            } else {
                // This message is for a process on another node - shouldn't happen
                tracing::warn!(?to, from_node = %from_node, "Received message for non-local PID");
            }
        }
        DistMessage::Ping { seq } => {
            // Respond with pong and update last seen
            if let Some(manager) = DIST_MANAGER.get()
                && let Some(node) = manager.nodes.get(&from_node)
            {
                node.touch();
                let _ = node.tx.try_send(DistMessage::Pong { seq });
            }
        }
        DistMessage::Pong { seq } => {
            // Update last seen timestamp - this is the critical heartbeat response
            if let Some(manager) = DIST_MANAGER.get()
                && let Some(node) = manager.nodes.get(&from_node)
            {
                node.touch();
            }
            tracing::trace!(seq, from_node = %from_node, "Received pong");
        }
        DistMessage::MonitorNode { requesting_pid } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager
                    .monitors
                    .add_remote_monitor(from_node, requesting_pid);
            }
        }
        DistMessage::DemonitorNode { requesting_pid } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager
                    .monitors
                    .remove_remote_monitor(from_node, requesting_pid);
            }
        }
        DistMessage::NodeGoingDown { reason } => {
            tracing::info!(from_node = %from_node, %reason, "Remote node going down");
            // The connection will close and trigger cleanup
        }
        DistMessage::GlobalRegistry { payload } => {
            // Handle global registry message
            if let Ok(msg) = postcard::from_bytes::<super::global::GlobalRegistryMessage>(&payload)
            {
                super::global::global_registry().handle_message(msg, from_node);
            }
        }
        DistMessage::ProcessGroups { payload } => {
            // Handle process groups message
            if let Ok(msg) = postcard::from_bytes::<super::pg::PgMessage>(&payload) {
                super::pg::pg().handle_message(msg, from_node);
            }
        }

        // === Process Monitoring ===
        DistMessage::MonitorProcess {
            from,
            target,
            reference,
        } => {
            if let Some(manager) = DIST_MANAGER.get() {
                let reference = Ref::from_raw(reference);
                manager
                    .process_monitors
                    .add_incoming_monitor(target, from, reference, from_node);

                // Check if target is alive - if not, send DOWN immediately
                if let Some(handle) = crate::process::global::try_handle()
                    && !handle.alive(target)
                {
                    // Target already dead - send ProcessDown
                    if let Some(tx) = manager.get_node_tx(from_node) {
                        let msg = DistMessage::ProcessDown {
                            reference: reference.as_raw(),
                            pid: target,
                            reason: "noproc".to_string(),
                        };
                        let _ = tx.try_send(msg);
                    }
                    manager
                        .process_monitors
                        .remove_incoming_monitor(target, from, reference);
                }
            }
        }
        DistMessage::DemonitorProcess {
            from,
            target,
            reference,
        } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager.process_monitors.remove_incoming_monitor(
                    target,
                    from,
                    Ref::from_raw(reference),
                );
            }
        }
        DistMessage::ProcessDown {
            reference,
            pid,
            reason,
        } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager.process_monitors.handle_process_down(
                    Ref::from_raw(reference),
                    pid,
                    &reason,
                );
            }
        }

        // === Process Linking ===
        DistMessage::Link { from, target } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager
                    .process_monitors
                    .handle_incoming_link(from, target, from_node);
            }
        }
        DistMessage::Unlink { from, target } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager
                    .process_monitors
                    .handle_incoming_unlink(from, target);
            }
        }
        DistMessage::Exit {
            from,
            target,
            reason,
        } => {
            if let Some(manager) = DIST_MANAGER.get() {
                manager
                    .process_monitors
                    .handle_incoming_exit(from, target, &reason);
            }
        }

        _ => {
            tracing::warn!(?msg, "Unexpected message type");
        }
    }
}

// === Public API Functions ===

/// Connect to a remote node.
///
/// Returns the remote node's name as an `Atom`.
///
/// # Example
///
/// ```ignore
/// let node = ambitious::dist::connect("192.168.1.100:9000").await?;
/// ```
pub async fn connect(addr: &str) -> Result<Atom, DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    let socket_addr: SocketAddr = addr
        .parse()
        .map_err(|e| DistError::InvalidAddress(format!("{}: {}", addr, e)))?;
    manager.connect_to(socket_addr).await
}

/// Disconnect from a node.
pub fn disconnect(node: Atom) -> Result<(), DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    manager.disconnect_from(node)
}

/// Get list of connected nodes.
pub fn nodes() -> Vec<Atom> {
    DIST_MANAGER
        .get()
        .map(|m| m.connected_nodes())
        .unwrap_or_default()
}

/// Get info about a connected node.
pub fn node_info(node: Atom) -> Option<NodeInfo> {
    DIST_MANAGER.get().and_then(|m| m.get_node_info(node))
}

/// Send a message to a remote process.
///
/// This is called by the process registry when sending to a non-local PID.
pub(crate) fn send_remote(pid: Pid, payload: Vec<u8>) -> Result<(), DistError> {
    let manager = DIST_MANAGER.get().ok_or(DistError::NotInitialized)?;
    manager.send_to_remote(pid, payload)
}

/// Heartbeat loop - periodically sends pings to all connected nodes and
/// detects dead nodes based on timeout.
///
/// This provides fast detection of node failures when the remote node crashes
/// without sending a graceful disconnect message.
async fn heartbeat_loop() {
    use std::sync::atomic::AtomicU64;
    static PING_SEQ: AtomicU64 = AtomicU64::new(0);

    loop {
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;

        let Some(manager) = DIST_MANAGER.get() else {
            continue;
        };

        // Collect nodes to check (avoid holding lock during iteration)
        let nodes_to_check: Vec<(Atom, bool)> = manager
            .nodes
            .iter()
            .map(|entry| (*entry.key(), entry.value().is_timed_out()))
            .collect();

        for (node_atom, is_timed_out) in nodes_to_check {
            if is_timed_out {
                // Node has timed out - disconnect it
                tracing::warn!(
                    node = %node_atom,
                    timeout_secs = HEARTBEAT_TIMEOUT.as_secs(),
                    "Node heartbeat timeout, disconnecting"
                );

                // Remove the node and trigger cleanup
                if let Some((_, node)) = manager.nodes.remove(&node_atom) {
                    tracing::info!(
                        node = %node_atom,
                        "Heartbeat: removed node, triggering cleanup"
                    );
                    node.connection.close("heartbeat timeout");
                    if let Some(addr) = node.info.addr {
                        manager.addr_to_node.remove(&addr);
                    }

                    // Notify node monitors
                    tracing::debug!(
                        node = %node_atom,
                        "Heartbeat: notifying node monitors"
                    );
                    manager
                        .monitors
                        .notify_node_down(node_atom, "heartbeat timeout".to_string());

                    // Clean up process monitors and links for this node
                    manager.process_monitors.handle_node_down(node_atom);

                    // Clean up pg memberships from this node
                    super::pg::pg().remove_node_members(node_atom);
                    tracing::info!(
                        node = %node_atom,
                        "Heartbeat: cleanup complete"
                    );
                } else {
                    tracing::warn!(
                        node = %node_atom,
                        "Heartbeat: node already removed by another path"
                    );
                }
            } else {
                // Node is alive - send ping
                if let Some(node) = manager.nodes.get(&node_atom) {
                    let seq = PING_SEQ.fetch_add(1, Ordering::Relaxed);
                    let _ = node.tx.try_send(DistMessage::Ping { seq });
                }
            }
        }
    }
}
