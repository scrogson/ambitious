//! DREAM Chat Server
//!
//! A multi-user chat application demonstrating DREAM's capabilities:
//! - Processes for user sessions
//! - GenServers for rooms and the registry
//! - Message passing between processes
//!
//! # Usage
//!
//! Start the server:
//! ```bash
//! cargo run --bin chat-server
//! ```
//!
//! Connect with netcat or the provided client:
//! ```bash
//! nc localhost 9999
//! ```
//!
//! # Protocol
//!
//! The protocol uses length-prefixed binary messages (postcard serialization).
//! For testing with netcat, use the simple text client instead.

mod protocol;
mod registry;
mod room;
mod server;
mod session;

use dream_process::Runtime;
use server::{run_acceptor, ServerConfig};
use std::env;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            env::var("RUST_LOG").unwrap_or_else(|_| "info,dream=debug".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting DREAM Chat Server");

    // Create the DREAM runtime
    let runtime = Runtime::new();
    let handle = runtime.handle();

    // Start the room registry GenServer
    let registry_pid = registry::start_registry(&handle).await?;
    tracing::info!(pid = ?registry_pid, "Registry started");

    // Configure and run the TCP server
    let config = ServerConfig::default();
    tracing::info!(addr = %config.addr, "Starting TCP acceptor");

    // Run the acceptor (this blocks)
    run_acceptor(handle, config, registry_pid).await?;

    Ok(())
}
