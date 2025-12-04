//! Node configuration and initialization.

use super::DIST_MANAGER;
use super::manager::{DistributionManager, send_remote};
use super::protocol::DistError;
use crate::core::Pid;
use crate::core::node::{NodeName, init_node};
use crate::runtime::SendError;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Configuration for distribution.
///
/// Use the builder pattern to configure, then call `start()` to initialize.
///
/// # Example
///
/// ```ignore
/// use ambitious::dist::Config;
///
/// Config::new()
///     .name("node1@localhost")
///     .listen_addr("0.0.0.0:9000")
///     .start()
///     .await?;
/// ```
#[derive(Clone, Debug)]
pub struct Config {
    /// Node name (e.g., "node1@localhost").
    pub name: Option<String>,
    /// Address to listen on for incoming connections.
    pub listen_addr: Option<String>,
    /// Path to TLS certificate file (optional, will generate self-signed if not provided).
    pub cert_path: Option<PathBuf>,
    /// Path to TLS private key file.
    pub key_path: Option<PathBuf>,
    /// Creation number (usually 0, incremented on restart).
    pub creation: u32,
}

impl Config {
    /// Create a new configuration with defaults.
    pub fn new() -> Self {
        Self {
            name: None,
            listen_addr: None,
            cert_path: None,
            key_path: None,
            creation: 0,
        }
    }

    /// Set the node name.
    ///
    /// Format: `name@host` (e.g., "node1@localhost").
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the address to listen on.
    ///
    /// Format: `host:port` (e.g., "0.0.0.0:9000").
    pub fn listen_addr(mut self, addr: impl Into<String>) -> Self {
        self.listen_addr = Some(addr.into());
        self
    }

    /// Set the path to the TLS certificate file.
    pub fn cert_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cert_path = Some(path.into());
        self
    }

    /// Set the path to the TLS private key file.
    pub fn key_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.key_path = Some(path.into());
        self
    }

    /// Set the creation number.
    pub fn creation(mut self, creation: u32) -> Self {
        self.creation = creation;
        self
    }

    /// Start distribution with this configuration.
    ///
    /// This initializes the distribution layer and starts listening for
    /// incoming connections if a listen address was configured.
    pub async fn start(self) -> Result<(), DistError> {
        init_distribution(self).await
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize the distribution layer.
///
/// This sets up the global distribution manager and optionally starts
/// listening for incoming connections.
pub async fn init_distribution(config: Config) -> Result<(), DistError> {
    // Determine the node name
    let node_name = config.name.clone().unwrap_or_else(|| {
        // Generate a default name based on hostname and PID
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        format!("dream_{}@{}", std::process::id(), hostname)
    });

    // Initialize node identity in dream-core
    let _ = init_node(NodeName::new(&node_name), config.creation);

    // Parse listen address if provided
    let listen_addr: Option<SocketAddr> = if let Some(ref addr) = config.listen_addr {
        Some(
            addr.parse()
                .map_err(|e| DistError::InvalidAddress(format!("{}: {}", addr, e)))?,
        )
    } else {
        None
    };

    // Create the distribution manager
    let manager = DistributionManager::new(node_name.clone(), config.creation);

    // Store it globally
    DIST_MANAGER
        .set(manager)
        .map_err(|_| DistError::Connect("distribution already initialized".to_string()))?;

    // Register the remote send hook with dream-runtime
    let _ = crate::runtime::set_remote_send_hook(remote_send_hook);

    // Start listening if an address was provided
    if let Some(addr) = listen_addr {
        let manager = DIST_MANAGER.get().unwrap();
        manager
            .start_listener(addr, config.cert_path, config.key_path)
            .await?;
    }

    tracing::info!(name = %node_name, "Distribution initialized");
    Ok(())
}

/// Hook function for sending to remote processes.
///
/// This is registered with dream-runtime to handle remote sends.
fn remote_send_hook(pid: Pid, data: Vec<u8>) -> Result<(), SendError> {
    send_remote(pid, data).map_err(|_| {
        // Convert DistError to SendError
        SendError::ProcessNotFound(pid)
    })
}
