//! DREAM Chat Server
//!
//! A multi-user chat application demonstrating DREAM's capabilities:
//! - Processes for user sessions
//! - GenServers for rooms and registry
//! - Message passing between processes
//!
//! # Usage
//!
//! Start the server:
//! ```bash
//! cargo run --bin chat-server
//! ```
//!
//! Connect with the provided client:
//! ```bash
//! cargo run --bin chat-client
//! ```
//!
//! # Protocol
//!
//! The protocol uses length-prefixed binary messages (postcard serialization).

mod protocol;
mod pubsub;
mod registry;
mod room;
mod server;
mod session;

use server::{run_acceptor, ServerConfig};
use std::env;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[dream::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            env::var("RUST_LOG").unwrap_or_else(|_| "info,dream=debug".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting DREAM Chat Server");

    // Start the PubSub server
    let pubsub_pid = pubsub::PubSub::start().await.expect("Failed to start pubsub");
    dream::register(pubsub::PubSub::NAME, pubsub_pid);
    tracing::info!(pid = ?pubsub_pid, "PubSub started and registered");

    // Start the room registry and register it by name
    let registry_pid = registry::Registry::start().await.expect("Failed to start registry");
    dream::register(registry::Registry::NAME, registry_pid);
    tracing::info!(pid = ?registry_pid, "Registry started and registered");

    // Configure and run the TCP server
    let config = ServerConfig::default();
    tracing::info!(addr = %config.addr, "Starting TCP acceptor");

    // Run the acceptor (this blocks)
    run_acceptor(config).await?;

    Ok(())
}
