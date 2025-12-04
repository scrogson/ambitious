//! Starlang Chat Server
//!
//! A multi-user chat application demonstrating Starlang's capabilities:
//! - Processes for user sessions
//! - GenServers for rooms and registry
//! - DynamicSupervisor for managing room processes
//! - Message passing between processes
//! - **Distribution**: Connect multiple chat servers together
//! - **pg**: Distributed process groups for room membership
//! - **WebSocket**: Phoenix Channels V2 protocol for web clients
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
//! Connect with the provided TUI client:
//! ```bash
//! cargo run --bin chat-client -- --port 9999
//! ```
//!
//! Or open the web interface at http://localhost:8080
//!
//! # Protocol
//!
//! - TCP: Length-prefixed binary messages (postcard serialization)
//! - WebSocket: Phoenix Channels V2 JSON protocol

#![deny(warnings)]
#![deny(missing_docs)]

mod channel;
mod http;
mod lobby;
mod protocol;
mod registry;
mod room;
mod room_supervisor;
mod server;
mod session;

use clap::Parser;
use server::{ServerConfig, run_acceptor};
use std::env;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Starlang Chat Server - A distributed chat application
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Node name (e.g., "node1")
    #[arg(short, long, default_value = "node1")]
    name: String,

    /// Port for client connections (TCP binary protocol)
    #[arg(short, long, default_value = "9999")]
    port: u16,

    /// Port for WebSocket connections (Phoenix Channels protocol)
    #[arg(short, long, default_value = "4000")]
    ws_port: u16,

    /// Port for HTTP server (serves web client)
    #[arg(long, default_value = "8080")]
    http_port: u16,

    /// Port for distribution (node-to-node connections)
    #[arg(short, long, default_value = "9000")]
    dist_port: u16,

    /// Connect to another node on startup (e.g., "127.0.0.1:9000")
    #[arg(short, long)]
    connect: Option<String>,
}

#[starlang::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            env::var("RUST_LOG").unwrap_or_else(|_| "info,starlang=debug".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!(
        name = %args.name,
        port = args.port,
        dist_port = args.dist_port,
        "Starting Starlang Chat Server"
    );

    // Initialize distribution
    let node_name = format!("{}@localhost", args.name);
    let dist_addr = format!("0.0.0.0:{}", args.dist_port);

    starlang::dist::Config::new()
        .name(&node_name)
        .listen_addr(&dist_addr)
        .start()
        .await
        .expect("Failed to start distribution");

    tracing::info!(node = %node_name, addr = %dist_addr, "Distribution started");

    // Connect to another node if specified
    if let Some(ref peer_addr) = args.connect {
        match starlang::dist::connect(peer_addr).await {
            Ok(node_id) => {
                tracing::info!(peer = %peer_addr, ?node_id, "Connected to peer node");
            }
            Err(e) => {
                tracing::error!(peer = %peer_addr, error = %e, "Failed to connect to peer node");
            }
        }
    }

    // Start PubSub server for distributed pub/sub messaging
    use starlang::pubsub::{PubSub, PubSubConfig};
    PubSub::start_link(PubSubConfig::new("chat_pubsub"))
        .await
        .expect("Failed to start PubSub");
    tracing::info!("PubSub started as 'chat_pubsub'");

    // Start Presence server for tracking user presence
    use starlang::presence::{Presence, PresenceConfig};
    Presence::start_link(PresenceConfig::new("chat_presence", "chat_pubsub"))
        .await
        .expect("Failed to start Presence");
    tracing::info!("Presence started as 'chat_presence'");

    // Start the room supervisor (DynamicSupervisor for managing room processes)
    let room_sup_pid = room_supervisor::start()
        .await
        .expect("Failed to start room supervisor");
    starlang::register(room_supervisor::NAME, room_sup_pid);
    tracing::info!(pid = ?room_sup_pid, "Room supervisor started and registered");

    // Start the room registry and register it by name
    let registry_pid = registry::Registry::start()
        .await
        .expect("Failed to start registry");
    starlang::register(registry::Registry::NAME, registry_pid);
    tracing::info!(pid = ?registry_pid, "Registry started and registered");

    // Configure and run the TCP server
    let client_addr = format!("127.0.0.1:{}", args.port);
    let config = ServerConfig {
        addr: client_addr.parse().unwrap(),
    };
    tracing::info!(addr = %config.addr, "Starting TCP acceptor for clients");

    // Start WebSocket server for Phoenix Channels clients
    let ws_addr: std::net::SocketAddr = format!("127.0.0.1:{}", args.ws_port).parse().unwrap();
    tracing::info!(addr = %ws_addr, "Starting WebSocket server (Phoenix Channels protocol)");

    let endpoint = starlang::channel::websocket::WebSocketEndpoint::new()
        .channel::<lobby::LobbyChannel>()
        .channel::<channel::RoomChannel>();

    tokio::spawn(async move {
        if let Err(e) = endpoint.listen(ws_addr).await {
            tracing::error!(error = %e, "WebSocket server error");
        }
    });

    // Start HTTP server for web client
    let http_addr: std::net::SocketAddr = format!("127.0.0.1:{}", args.http_port).parse().unwrap();
    tracing::info!(addr = %http_addr, "Starting HTTP server (web client at http://{})", http_addr);

    tokio::spawn(async move {
        if let Err(e) = http::serve(http_addr).await {
            tracing::error!(error = %e, "HTTP server error");
        }
    });

    // Run the TCP acceptor (this blocks)
    run_acceptor(config).await?;

    Ok(())
}
