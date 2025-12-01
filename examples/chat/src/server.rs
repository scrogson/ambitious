//! TCP server for accepting chat client connections.

use crate::session::spawn_session;
use dream_core::Pid;
use dream_process::RuntimeHandle;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// TCP server configuration.
pub struct ServerConfig {
    /// Address to bind to.
    pub addr: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:9999".parse().unwrap(),
        }
    }
}

/// Run the TCP acceptor loop.
///
/// This accepts incoming connections and spawns a session process for each.
pub async fn run_acceptor(
    handle: RuntimeHandle,
    config: ServerConfig,
    registry_pid: Pid,
) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %config.addr, "Chat server listening");

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                tracing::info!(peer = %peer_addr, "New connection");

                // Spawn a session process for this client
                let session_pid = spawn_session(&handle, stream, registry_pid);
                tracing::debug!(pid = ?session_pid, peer = %peer_addr, "Session spawned");
            }
            Err(e) => {
                tracing::error!(error = %e, "Accept failed");
            }
        }
    }
}
