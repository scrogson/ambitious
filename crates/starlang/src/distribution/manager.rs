//! Distribution connection manager.
//!
//! Manages connections to remote nodes and routes messages.
//! Nodes are identified by their name (as an Atom) for globally unique addressing.

use super::DIST_MANAGER;
use super::monitor::NodeMonitorRegistry;
use super::protocol::{DistError, DistMessage};
use super::transport::{QuicConnection, QuicTransport};
use crate::core::{Atom, NodeInfo, NodeName, Pid};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Information about a connected node.
struct ConnectedNode {
    /// The node's info.
    info: NodeInfo,
    /// The QUIC connection.
    connection: QuicConnection,
    /// Sender for outgoing messages.
    tx: mpsc::Sender<DistMessage>,
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
    /// The QUIC transport (if listening).
    transport: RwLock<Option<Arc<QuicTransport>>>,
    /// Node monitor registry.
    monitors: NodeMonitorRegistry,
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
            transport: RwLock::new(None),
            monitors: NodeMonitorRegistry::new(),
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
            ConnectedNode {
                info: node_info,
                connection,
                tx,
            },
        );
        self.addr_to_node.insert(addr, remote_node_atom);

        // Spawn message sender task
        let node_atom = remote_node_atom;
        tokio::spawn(async move {
            message_sender_loop(rx, node_atom).await;
        });

        // Spawn message receiver task
        let node_atom = remote_node_atom;
        tokio::spawn(async move {
            message_receiver_loop(node_atom).await;
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

            // Notify monitors
            self.monitors
                .notify_node_down(node_atom, "disconnect requested".to_string());

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
            Err(DistError::NotConnected(node_atom))
        }
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

    manager.nodes.insert(
        remote_node_atom,
        ConnectedNode {
            info,
            connection,
            tx,
        },
    );
    manager.addr_to_node.insert(addr, remote_node_atom);

    // Spawn message handling tasks
    let node_atom = remote_node_atom;
    tokio::spawn(async move {
        message_sender_loop(rx, node_atom).await;
    });

    // Spawn receiver loop
    let node_atom = remote_node_atom;
    tokio::spawn(async move {
        message_receiver_loop(node_atom).await;
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
async fn message_sender_loop(mut rx: mpsc::Receiver<DistMessage>, node_atom: Atom) {
    while let Some(msg) = rx.recv().await {
        let manager = match DIST_MANAGER.get() {
            Some(m) => m,
            None => break,
        };

        if let Some(node) = manager.nodes.get(&node_atom) {
            if let Err(e) = node.connection.send_message(&msg).await {
                tracing::error!(error = %e, node = %node_atom, "Failed to send message");
                break;
            }
        } else {
            break;
        }
    }

    // Connection closed or error - clean up
    if let Some(manager) = DIST_MANAGER.get()
        && let Some((_, node)) = manager.nodes.remove(&node_atom) {
            if let Some(addr) = node.info.addr {
                manager.addr_to_node.remove(&addr);
            }
            manager
                .monitors
                .notify_node_down(node_atom, "connection closed".to_string());
            // Clean up pg memberships from this node
            super::pg::pg().remove_node_members(node_atom);
        }
}

/// Loop to receive messages from a remote node.
async fn message_receiver_loop(node_atom: Atom) {
    while let Some(manager) = DIST_MANAGER.get() {
        let connection = match manager.nodes.get(&node_atom) {
            Some(node) => {
                // Accept a stream
                match node.connection.accept_stream().await {
                    Ok((_, recv)) => recv,
                    Err(_) => break,
                }
            }
            None => break,
        };

        // We need to drop the borrow before calling recv_message
        let mut recv = connection;
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

    // Clean up
    if let Some(manager) = DIST_MANAGER.get()
        && let Some((_, node)) = manager.nodes.remove(&node_atom) {
            if let Some(addr) = node.info.addr {
                manager.addr_to_node.remove(&addr);
            }
            manager
                .monitors
                .notify_node_down(node_atom, "connection closed".to_string());
            // Clean up pg memberships from this node
            super::pg::pg().remove_node_members(node_atom);
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
            // Respond with pong
            if let Some(manager) = DIST_MANAGER.get()
                && let Some(node) = manager.nodes.get(&from_node) {
                    let _ = node.tx.try_send(DistMessage::Pong { seq });
                }
        }
        DistMessage::Pong { seq } => {
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
/// let node = starlang::dist::connect("192.168.1.100:9000").await?;
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
