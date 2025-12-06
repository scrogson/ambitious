//! Erlang Distribution Protocol support for BEAM interoperability.
//!
//! This module enables Ambitious nodes to connect to and communicate with
//! Erlang/Elixir nodes using the native Erlang Distribution Protocol.
//!
//! # Architecture
//!
//! Unlike the QUIC/TCP transports which use our own postcard-based protocol,
//! this module uses `edp_client` to implement the full Erlang distribution
//! protocol including:
//!
//! - EPMD (Erlang Port Mapper Daemon) for node discovery
//! - Erlang distribution handshake (challenge/response)
//! - Erlang Term Format (ETF) for message encoding via `erltf`
//! - Atom caching for efficient serialization
//! - Process linking and monitoring with BEAM semantics
//!
//! # Example
//!
//! ```ignore
//! use ambitious::distribution::erlang::{ErlangConnection, ErlangConfig};
//!
//! // Connect to an Erlang node
//! let config = ErlangConfig::new("rust@localhost", "erlang@localhost", "secret_cookie");
//! let conn = ErlangConnection::connect(config).await?;
//!
//! // Send a message to a registered process
//! conn.send_to_name(from_pid, "my_server", my_message).await?;
//! ```
//!
//! # Feature Flag
//!
//! This module is only available when the `erlang-dist` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! ambitious = { version = "0.1", features = ["erlang-dist"] }
//! ```

#[cfg(feature = "erlang-dist")]
mod inner {
    use super::super::protocol::DistError;
    use crate::core::Pid;
    pub use edp_client::control::ControlMessage;
    pub use edp_client::{Connection, ConnectionConfig, DistributionFlags};
    use erltf::OwnedTerm;
    use erltf::types::{Atom, ExternalPid, ExternalReference};

    /// Configuration for connecting to an Erlang node.
    #[derive(Debug, Clone)]
    pub struct ErlangConfig {
        /// Our node name (e.g., "rust@localhost").
        pub local_node: String,
        /// Remote node name (e.g., "erlang@localhost").
        pub remote_node: String,
        /// The shared cookie for authentication.
        pub cookie: String,
        /// Optional EPMD host (defaults to localhost).
        pub epmd_host: Option<String>,
        /// Connection timeout in seconds.
        pub timeout_secs: u64,
    }

    impl ErlangConfig {
        /// Create a new Erlang connection config.
        pub fn new(local_node: &str, remote_node: &str, cookie: &str) -> Self {
            Self {
                local_node: local_node.to_string(),
                remote_node: remote_node.to_string(),
                cookie: cookie.to_string(),
                epmd_host: None,
                timeout_secs: 30,
            }
        }

        /// Set the EPMD host.
        pub fn with_epmd_host(mut self, host: &str) -> Self {
            self.epmd_host = Some(host.to_string());
            self
        }

        /// Set the connection timeout.
        pub fn with_timeout(mut self, secs: u64) -> Self {
            self.timeout_secs = secs;
            self
        }
    }

    /// A connection to an Erlang/BEAM node.
    ///
    /// This wraps `edp_client::Connection` and provides a more ergonomic API
    /// that integrates with Ambitious's process model.
    pub struct ErlangConnection {
        /// The underlying edp_client connection.
        inner: Connection,
        /// Our node name.
        local_node: String,
        /// Remote node name.
        remote_node: String,
        /// Counter for generating unique IDs.
        id_counter: std::sync::atomic::AtomicU64,
    }

    impl ErlangConnection {
        /// Connect to an Erlang node using the provided configuration.
        pub async fn connect(config: ErlangConfig) -> Result<Self, DistError> {
            let conn_config =
                ConnectionConfig::new(&config.local_node, &config.remote_node, &config.cookie);

            // Apply optional settings
            let conn_config = if let Some(ref host) = config.epmd_host {
                conn_config.with_epmd_host(host)
            } else {
                conn_config
            };

            let mut connection = Connection::new(conn_config);

            connection
                .connect()
                .await
                .map_err(|e| DistError::Connect(format!("Erlang connection failed: {}", e)))?;

            tracing::info!(
                local = %config.local_node,
                remote = %config.remote_node,
                "Connected to Erlang node"
            );

            Ok(Self {
                inner: connection,
                local_node: config.local_node,
                remote_node: config.remote_node,
                id_counter: std::sync::atomic::AtomicU64::new(1),
            })
        }

        /// Send a message to a process by PID.
        ///
        /// The message will be encoded to ETF before sending.
        pub async fn send_to_pid(
            &mut self,
            from: &ErlangPid,
            to: &ErlangPid,
            message: OwnedTerm,
        ) -> Result<(), DistError> {
            self.inner
                .send_message(from.0.clone(), to.0.clone(), message)
                .await
                .map_err(|e| DistError::Io(format!("send failed: {}", e)))
        }

        /// Send a message to a registered process by name.
        ///
        /// The message will be encoded to ETF before sending.
        pub async fn send_to_name(
            &mut self,
            from: &ErlangPid,
            name: &str,
            message: OwnedTerm,
        ) -> Result<(), DistError> {
            self.inner
                .send_to_name(from.0.clone(), Atom::new(name), message)
                .await
                .map_err(|e| DistError::Io(format!("send_to_name failed: {}", e)))
        }

        /// Receive the next message from the connection.
        ///
        /// Returns the control message and optional payload.
        pub async fn receive(&mut self) -> Result<ErlangMessage, DistError> {
            let (control, payload) = self
                .inner
                .receive_message()
                .await
                .map_err(|e| DistError::Io(format!("receive failed: {}", e)))?;

            Ok(ErlangMessage { control, payload })
        }

        /// Link two processes across the distribution boundary.
        pub async fn link(&mut self, from: &ErlangPid, to: &ErlangPid) -> Result<(), DistError> {
            self.inner
                .link(&from.0, &to.0)
                .await
                .map_err(|e| DistError::Io(format!("link failed: {}", e)))
        }

        /// Unlink two processes.
        pub async fn unlink(
            &mut self,
            from: &ErlangPid,
            to: &ErlangPid,
            unlink_id: u64,
        ) -> Result<(), DistError> {
            self.inner
                .unlink(&from.0, &to.0, unlink_id)
                .await
                .map_err(|e| DistError::Io(format!("unlink failed: {}", e)))
        }

        /// Monitor a remote process.
        pub async fn monitor(
            &mut self,
            from: &ErlangPid,
            to: &ErlangPid,
            reference: &ErlangRef,
        ) -> Result<(), DistError> {
            self.inner
                .monitor(&from.0, &to.0, &reference.0)
                .await
                .map_err(|e| DistError::Io(format!("monitor failed: {}", e)))
        }

        /// Remove a monitor.
        pub async fn demonitor(
            &mut self,
            from: &ErlangPid,
            to: &ErlangPid,
            reference: &ErlangRef,
        ) -> Result<(), DistError> {
            self.inner
                .demonitor(&from.0, &to.0, &reference.0)
                .await
                .map_err(|e| DistError::Io(format!("demonitor failed: {}", e)))
        }

        /// Check if the connection is still active.
        pub fn is_connected(&self) -> bool {
            self.inner.is_connected()
        }

        /// Close the connection.
        pub async fn close(&mut self) -> Result<(), DistError> {
            self.inner
                .close()
                .await
                .map_err(|e| DistError::Io(format!("close failed: {}", e)))
        }

        /// Get our local node name.
        pub fn local_node(&self) -> &str {
            &self.local_node
        }

        /// Get the remote node name.
        pub fn remote_node(&self) -> &str {
            &self.remote_node
        }

        /// Allocate a new PID for a local process.
        ///
        /// This creates an `ExternalPid` that can be used to send messages
        /// as a specific process to the Erlang node.
        pub fn allocate_pid(&self) -> ErlangPid {
            let id = self
                .id_counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ErlangPid(ExternalPid::new(
                Atom::new(&self.local_node),
                id as u32,
                0,
                1,
            ))
        }

        /// Allocate a new reference for monitors.
        pub fn allocate_ref(&self) -> ErlangRef {
            let id = self
                .id_counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ErlangRef(ExternalReference::new(
                Atom::new(&self.local_node),
                1,
                vec![id as u32],
            ))
        }
    }

    /// An Erlang PID wrapper for type safety.
    #[derive(Debug, Clone)]
    pub struct ErlangPid(ExternalPid);

    impl ErlangPid {
        /// Create from an erltf ExternalPid.
        pub fn new(pid: ExternalPid) -> Self {
            Self(pid)
        }

        /// Get the inner ExternalPid.
        pub fn into_inner(self) -> ExternalPid {
            self.0
        }

        /// Get a reference to the inner ExternalPid.
        pub fn as_inner(&self) -> &ExternalPid {
            &self.0
        }

        /// Convert from an Ambitious Pid.
        ///
        /// Note: This creates a representation suitable for sending to Erlang,
        /// but the Erlang node won't be able to send back to this PID directly
        /// without proper integration.
        pub fn from_ambitious(pid: Pid, node_name: &str, creation: u32) -> Self {
            Self(ExternalPid::new(
                Atom::new(node_name),
                pid.id() as u32,
                0,
                creation,
            ))
        }
    }

    /// An Erlang reference wrapper.
    #[derive(Debug, Clone)]
    pub struct ErlangRef(ExternalReference);

    impl ErlangRef {
        /// Create from an erltf ExternalReference.
        pub fn new(reference: ExternalReference) -> Self {
            Self(reference)
        }

        /// Get the inner reference.
        pub fn into_inner(self) -> ExternalReference {
            self.0
        }

        /// Get a reference to the inner ExternalReference.
        pub fn as_inner(&self) -> &ExternalReference {
            &self.0
        }
    }

    /// A message received from an Erlang node.
    #[derive(Debug)]
    pub struct ErlangMessage {
        /// The control message (describes the operation).
        pub control: ControlMessage,
        /// The optional payload (the actual message data).
        pub payload: Option<OwnedTerm>,
    }

    impl ErlangMessage {
        /// Get a reference to the payload if present.
        pub fn payload(&self) -> Option<&OwnedTerm> {
            self.payload.as_ref()
        }

        /// Take the payload, leaving None in its place.
        pub fn take_payload(&mut self) -> Option<OwnedTerm> {
            self.payload.take()
        }
    }
}

// Re-export when feature is enabled
#[cfg(feature = "erlang-dist")]
pub use inner::*;

// Provide a helpful error when feature is not enabled
#[cfg(not(feature = "erlang-dist"))]
/// Placeholder when erlang-dist feature is not enabled.
///
/// Enable the `erlang-dist` feature to use Erlang distribution:
/// ```toml
/// ambitious = { features = ["erlang-dist"] }
/// ```
pub fn connect(_config: ()) -> Result<(), super::protocol::DistError> {
    Err(super::protocol::DistError::NotInitialized)
}
