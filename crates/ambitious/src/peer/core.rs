//! Peer node implementation.

use super::options::{Connection, PeerOptions, SpawnMethod};
use crate::atom::Atom;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;

/// Counter for generating unique peer names.
static PEER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Error type for peer operations.
#[derive(Debug, Clone)]
pub enum PeerError {
    /// Failed to spawn the peer process.
    SpawnFailed(String),
    /// Peer process exited unexpectedly.
    ProcessExited(Option<i32>),
    /// Timeout waiting for peer to boot.
    BootTimeout,
    /// Failed to connect to peer.
    ConnectionFailed(String),
    /// Peer is not running.
    NotRunning,
    /// Distribution error.
    DistributionError(String),
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed(e) => write!(f, "failed to spawn peer: {}", e),
            Self::ProcessExited(code) => match code {
                Some(c) => write!(f, "peer process exited with code {}", c),
                None => write!(f, "peer process was terminated"),
            },
            Self::BootTimeout => write!(f, "timeout waiting for peer to boot"),
            Self::ConnectionFailed(e) => write!(f, "failed to connect to peer: {}", e),
            Self::NotRunning => write!(f, "peer is not running"),
            Self::DistributionError(e) => write!(f, "distribution error: {}", e),
        }
    }
}

impl std::error::Error for PeerError {}

/// State of a peer node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// Peer process is starting.
    Booting,
    /// Peer is running and connected.
    Running,
    /// Peer has stopped.
    Down,
}

/// A managed peer node.
///
/// Represents a peer Ambitious node that was spawned by this process.
/// The peer can be controlled via RPC and will be automatically
/// cleaned up when dropped (if started with `start_link`).
pub struct Peer {
    /// The peer's node name atom.
    node: Atom,
    /// The full node name string (e.g., "peer1@localhost").
    node_name: String,
    /// The child process handle.
    child: Arc<RwLock<Option<Child>>>,
    /// Current state of the peer.
    state: Arc<RwLock<PeerState>>,
    /// Options used to start this peer.
    options: PeerOptions,
    /// Whether this peer should be killed on drop.
    kill_on_drop: bool,
    /// Distribution port the peer is listening on.
    dist_port: u16,
}

impl Peer {
    /// Start a new peer node.
    ///
    /// The peer is spawned as a child process and will be monitored.
    /// Use `wait_boot` to wait for the peer to be ready.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let peer = Peer::start(PeerOptions::new().name("test")).await?;
    /// peer.wait_boot(Duration::from_secs(10)).await?;
    /// ```
    pub async fn start(options: PeerOptions) -> Result<Self, PeerError> {
        Self::start_impl(options, false).await
    }

    /// Start a new peer node linked to the current process.
    ///
    /// Like `start`, but the peer will be automatically stopped when
    /// the `Peer` handle is dropped.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let peer = Peer::start_link(PeerOptions::new().name("test")).await?;
    /// // Peer is automatically stopped when `peer` goes out of scope
    /// ```
    pub async fn start_link(options: PeerOptions) -> Result<Self, PeerError> {
        Self::start_impl(options, true).await
    }

    async fn start_impl(options: PeerOptions, kill_on_drop: bool) -> Result<Self, PeerError> {
        // Generate node name if not provided
        let peer_name = options.name.clone().unwrap_or_else(|| {
            let id = PEER_COUNTER.fetch_add(1, Ordering::SeqCst);
            format!("peer{}", id)
        });

        let host = options.host.clone().unwrap_or_else(|| "localhost".into());
        let node_name = format!("{}@{}", peer_name, host);

        // Determine distribution port
        let dist_port = options.dist_port.unwrap_or_else(|| {
            // Find an available port
            std::net::TcpListener::bind("127.0.0.1:0")
                .map(|l| l.local_addr().unwrap().port())
                .unwrap_or(0)
        });

        // Build the command
        let mut cmd = match &options.spawn_method {
            SpawnMethod::CurrentExecutable => {
                let exe = std::env::current_exe().map_err(|e| {
                    PeerError::SpawnFailed(format!("cannot get current exe: {}", e))
                })?;
                Command::new(exe)
            }
            SpawnMethod::Executable(path) => Command::new(path),
            SpawnMethod::Ssh {
                host,
                user,
                remote_executable,
            } => {
                let mut cmd = Command::new("ssh");
                if let Some(u) = user {
                    cmd.arg("-l").arg(u);
                }
                cmd.arg(host);
                cmd.arg(remote_executable);
                cmd
            }
            SpawnMethod::Custom {
                command,
                command_args,
            } => {
                let mut cmd = Command::new(command);
                cmd.args(command_args);
                cmd
            }
        };

        // Add standard distribution arguments
        cmd.arg("--name").arg(&peer_name);
        cmd.arg("--dist-port").arg(dist_port.to_string());

        // Add configured ports
        for (name, port) in &options.ports {
            cmd.arg(format!("--{}", name)).arg(port.to_string());
        }

        // Add user-provided arguments
        cmd.args(&options.args);

        // Set environment variables
        for (key, value) in &options.env {
            cmd.env(key, value);
        }

        // Configure stdio based on connection type
        match &options.connection {
            Connection::Stdio => {
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::inherit());
            }
            _ => {
                cmd.stdin(Stdio::null());
                cmd.stdout(Stdio::inherit());
                cmd.stderr(Stdio::inherit());
            }
        }

        // Enable kill on drop
        cmd.kill_on_drop(kill_on_drop);

        // Spawn the process
        let child = cmd
            .spawn()
            .map_err(|e| PeerError::SpawnFailed(e.to_string()))?;

        let node = Atom::from(node_name.as_str());

        Ok(Self {
            node,
            node_name,
            child: Arc::new(RwLock::new(Some(child))),
            state: Arc::new(RwLock::new(PeerState::Booting)),
            options,
            kill_on_drop,
            dist_port,
        })
    }

    /// Get the peer's node name as an atom.
    pub fn node(&self) -> Atom {
        self.node
    }

    /// Get the peer's full node name string.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Get the current state of the peer.
    pub async fn get_state(&self) -> PeerState {
        *self.state.read().await
    }

    /// Get the distribution port the peer is listening on.
    pub fn dist_port(&self) -> u16 {
        self.dist_port
    }

    /// Get the distribution address for connecting to this peer.
    pub fn dist_addr(&self) -> String {
        let host = self.options.host.as_deref().unwrap_or("localhost");
        format!("{}:{}", host, self.dist_port)
    }

    /// Wait for the peer to finish booting and become ready.
    ///
    /// This waits for the distribution connection to be established.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for boot. If `None`, uses the
    ///   timeout from `PeerOptions`.
    pub async fn wait_boot(&self, timeout: Option<Duration>) -> Result<(), PeerError> {
        let timeout = timeout.or(self.options.boot_timeout);
        let start = std::time::Instant::now();

        let dist_addr = self.dist_addr();

        loop {
            // Check if process is still running
            {
                let mut child_guard = self.child.write().await;
                if let Some(ref mut child_proc) = *child_guard {
                    match child_proc.try_wait() {
                        Ok(Some(status)) => {
                            *self.state.write().await = PeerState::Down;
                            return Err(PeerError::ProcessExited(status.code()));
                        }
                        Ok(None) => {} // Still running
                        Err(e) => {
                            return Err(PeerError::SpawnFailed(format!(
                                "failed to check process status: {}",
                                e
                            )));
                        }
                    }
                }
            }

            // Try to connect via distribution
            match crate::dist::connect(&dist_addr).await {
                Ok(_) => {
                    *self.state.write().await = PeerState::Running;
                    return Ok(());
                }
                Err(_) => {
                    // Not ready yet, check timeout
                    if let Some(t) = timeout
                        && start.elapsed() > t
                    {
                        return Err(PeerError::BootTimeout);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Stop the peer node.
    ///
    /// This sends a termination signal to the peer process.
    pub async fn stop(&self) -> Result<(), PeerError> {
        let mut child = self.child.write().await;

        if let Some(mut c) = child.take() {
            // Try graceful shutdown first via distribution
            if *self.state.read().await == PeerState::Running {
                // TODO: Send shutdown message via distribution
            }

            // Kill the process
            let _ = c.kill().await;
            let _ = c.wait().await;
        }

        *self.state.write().await = PeerState::Down;
        Ok(())
    }

    /// Check if the peer process is still running.
    pub async fn is_alive(&self) -> bool {
        let mut child_guard = self.child.write().await;
        if let Some(ref mut child_proc) = *child_guard {
            match child_proc.try_wait() {
                Ok(Some(_)) => {
                    *self.state.write().await = PeerState::Down;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if self.kill_on_drop {
            // Note: The actual killing is handled by tokio's Command::kill_on_drop
            // This is just for logging
            tracing::debug!(node = %self.node_name, "Peer dropped, process will be killed");
        }
    }
}

/// Generate a random node name suitable for a peer.
///
/// Returns a name like "peer_abc123@localhost".
pub fn random_name() -> String {
    random_name_with_prefix("peer")
}

/// Generate a random node name with a custom prefix.
pub fn random_name_with_prefix(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = PEER_COUNTER.fetch_add(1, Ordering::SeqCst);

    format!("{}_{}_{}", prefix, id, timestamp % 1000000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_name() {
        let name1 = random_name();
        let name2 = random_name();

        assert!(name1.starts_with("peer_"));
        assert!(name2.starts_with("peer_"));
        assert_ne!(name1, name2);
    }

    #[test]
    fn test_random_name_with_prefix() {
        let name = random_name_with_prefix("test");
        assert!(name.starts_with("test_"));
    }

    #[test]
    fn test_peer_options_builder() {
        let opts = PeerOptions::new()
            .name("mynode")
            .host("example.com")
            .dist_port(9000)
            .arg("--verbose")
            .env("RUST_LOG", "debug");

        assert_eq!(opts.name, Some("mynode".into()));
        assert_eq!(opts.host, Some("example.com".into()));
        assert_eq!(opts.dist_port, Some(9000));
        assert!(opts.args.contains(&"--verbose".to_string()));
        assert_eq!(opts.env.get("RUST_LOG"), Some(&"debug".to_string()));
    }
}
