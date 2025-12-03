//! DREAM Chat Server
//!
//! A multi-user chat application demonstrating DREAM's capabilities:
//! - Processes for user sessions
//! - GenServers for rooms and registry
//! - DynamicSupervisor for managing room processes
//! - Message passing between processes
//! - **Distribution**: Connect multiple chat servers together
//! - **pg**: Distributed process groups for room membership
//!
//! # Usage
//!
//! Start the first server:
//! ```bash
//! cargo run --bin chat-server -- --name node1 --port 9999 --dist-port 9000
//! ```
//!
//! Start a second server and connect to the first:
//! ```bash
//! cargo run --bin chat-server -- --name node2 --port 9998 --dist-port 9001 --connect 127.0.0.1:9000
//! ```
//!
//! Connect with the provided client:
//! ```bash
//! cargo run --bin chat-client -- --port 9999
//! ```
//!
//! # Protocol
//!
//! The protocol uses length-prefixed binary messages (postcard serialization).

#![deny(warnings)]
#![deny(missing_docs)]

mod channel;
mod protocol;
mod pubsub;
mod registry;
mod room;
mod room_supervisor;
mod server;
mod session;

// PubSub module is available for use but not currently used in main
#[allow(unused_imports)]
use pubsub::PubSub;

use clap::Parser;
use server::{run_acceptor, ServerConfig};
use std::env;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// DREAM Chat Server - A distributed chat application
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Node name (e.g., "node1")
    #[arg(short, long, default_value = "node1")]
    name: String,

    /// Port for client connections
    #[arg(short, long, default_value = "9999")]
    port: u16,

    /// Port for distribution (node-to-node connections)
    #[arg(short, long, default_value = "9000")]
    dist_port: u16,

    /// Connect to another node on startup (e.g., "127.0.0.1:9000")
    #[arg(short, long)]
    connect: Option<String>,
}

#[dream::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            env::var("RUST_LOG").unwrap_or_else(|_| "info,dream=debug".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!(
        name = %args.name,
        port = args.port,
        dist_port = args.dist_port,
        "Starting DREAM Chat Server"
    );

    // Initialize distribution
    let node_name = format!("{}@localhost", args.name);
    let dist_addr = format!("0.0.0.0:{}", args.dist_port);

    dream::dist::Config::new()
        .name(&node_name)
        .listen_addr(&dist_addr)
        .start()
        .await
        .expect("Failed to start distribution");

    tracing::info!(node = %node_name, addr = %dist_addr, "Distribution started");

    // Connect to another node if specified
    if let Some(ref peer_addr) = args.connect {
        match dream::dist::connect(peer_addr).await {
            Ok(node_id) => {
                tracing::info!(peer = %peer_addr, ?node_id, "Connected to peer node");
            }
            Err(e) => {
                tracing::error!(peer = %peer_addr, error = %e, "Failed to connect to peer node");
            }
        }
    }

    // PubSub is now stateless (uses pg under the hood)
    // No need to start a GenServer

    // Start the room supervisor (DynamicSupervisor for managing room processes)
    let room_sup_pid = room_supervisor::start().await.expect("Failed to start room supervisor");
    dream::register(room_supervisor::NAME, room_sup_pid);
    tracing::info!(pid = ?room_sup_pid, "Room supervisor started and registered");

    // Start the room registry and register it by name
    let registry_pid = registry::Registry::start().await.expect("Failed to start registry");
    dream::register(registry::Registry::NAME, registry_pid);
    tracing::info!(pid = ?registry_pid, "Registry started and registered");

    // Configure and run the TCP server
    let client_addr = format!("127.0.0.1:{}", args.port);
    let config = ServerConfig {
        addr: client_addr.parse().unwrap(),
    };
    tracing::info!(addr = %config.addr, "Starting TCP acceptor for clients");

    // Run the acceptor (this blocks)
    run_acceptor(config).await?;

    Ok(())
}
