//! Configuration options for peer nodes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Method used to spawn the peer process.
#[derive(Debug, Clone, Default)]
pub enum SpawnMethod {
    /// Spawn directly using the current executable.
    ///
    /// This is the default method for local testing.
    #[default]
    CurrentExecutable,

    /// Spawn using a specific executable path.
    Executable(PathBuf),

    /// Spawn via SSH to a remote host.
    ///
    /// The peer will be started on the remote host using SSH.
    Ssh {
        /// Remote host to connect to.
        host: String,
        /// SSH user (defaults to current user).
        user: Option<String>,
        /// Path to the executable on the remote host.
        remote_executable: PathBuf,
    },

    /// Spawn using a custom command.
    ///
    /// This allows for spawning in containers (Docker), via systemd, etc.
    Custom {
        /// The command to run.
        command: String,
        /// Arguments to the command (the executable and args are appended).
        command_args: Vec<String>,
    },
}

/// Connection type between origin and peer.
#[derive(Debug, Clone, Default)]
pub enum Connection {
    /// Standard Ambitious distribution (QUIC-based).
    ///
    /// The peer connects back to the origin via the distribution layer.
    #[default]
    Standard,

    /// TCP control connection.
    ///
    /// A dedicated TCP connection for control messages.
    /// Useful when distribution isn't available.
    Tcp {
        /// Port to listen on for control connection.
        port: u16,
    },

    /// Standard I/O connection.
    ///
    /// Control messages are sent via stdin/stdout.
    /// Useful for debugging and simple setups.
    Stdio,
}

/// Behavior when the control connection is lost.
#[derive(Debug, Clone, Default)]
pub enum ShutdownBehavior {
    /// Terminate the peer immediately (default).
    #[default]
    Halt,

    /// Continue running (peer becomes standalone).
    Continue,

    /// Attempt to reconnect for the specified duration before halting.
    Reconnect(Duration),
}

/// Options for starting a peer node.
#[derive(Debug, Clone)]
pub struct PeerOptions {
    /// Node name for the peer.
    ///
    /// If not specified, a random name will be generated.
    pub name: Option<String>,

    /// Host part of the node name.
    ///
    /// Defaults to "localhost" for local peers.
    pub host: Option<String>,

    /// Arguments to pass to the peer executable.
    ///
    /// These are passed after the executable name.
    pub args: Vec<String>,

    /// Environment variables to set for the peer process.
    pub env: HashMap<String, String>,

    /// Method used to spawn the peer.
    pub spawn_method: SpawnMethod,

    /// Connection type for control messages.
    pub connection: Connection,

    /// Behavior when control connection is lost.
    pub shutdown: ShutdownBehavior,

    /// Timeout for waiting for the peer to boot.
    ///
    /// If `None`, `wait_boot` will wait indefinitely.
    pub boot_timeout: Option<Duration>,

    /// Port for the peer's distribution listener.
    ///
    /// If not specified, a random available port will be used.
    pub dist_port: Option<u16>,

    /// Additional ports to configure (e.g., WebSocket, HTTP).
    ///
    /// These are passed as `--{key}` arguments.
    pub ports: HashMap<String, u16>,
}

impl Default for PeerOptions {
    fn default() -> Self {
        Self {
            name: None,
            host: Some("localhost".into()),
            args: Vec::new(),
            env: HashMap::new(),
            spawn_method: SpawnMethod::default(),
            connection: Connection::default(),
            shutdown: ShutdownBehavior::default(),
            boot_timeout: Some(Duration::from_secs(30)),
            dist_port: None,
            ports: HashMap::new(),
        }
    }
}

impl PeerOptions {
    /// Create new peer options with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the peer node name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the host part of the node name.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Add an argument to pass to the peer.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments to pass to the peer.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable for the peer process.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set the spawn method.
    pub fn spawn_method(mut self, method: SpawnMethod) -> Self {
        self.spawn_method = method;
        self
    }

    /// Set the connection type.
    pub fn connection(mut self, conn: Connection) -> Self {
        self.connection = conn;
        self
    }

    /// Set the shutdown behavior.
    pub fn shutdown(mut self, behavior: ShutdownBehavior) -> Self {
        self.shutdown = behavior;
        self
    }

    /// Set the boot timeout.
    pub fn boot_timeout(mut self, timeout: Duration) -> Self {
        self.boot_timeout = Some(timeout);
        self
    }

    /// Set no boot timeout (wait indefinitely).
    pub fn no_boot_timeout(mut self) -> Self {
        self.boot_timeout = None;
        self
    }

    /// Set the distribution port.
    pub fn dist_port(mut self, port: u16) -> Self {
        self.dist_port = Some(port);
        self
    }

    /// Set an additional port configuration.
    pub fn port(mut self, name: impl Into<String>, port: u16) -> Self {
        self.ports.insert(name.into(), port);
        self
    }

    /// Use a specific executable instead of the current one.
    pub fn executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.spawn_method = SpawnMethod::Executable(path.into());
        self
    }
}
