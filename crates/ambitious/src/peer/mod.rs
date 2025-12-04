//! Peer node management for Ambitious.
//!
//! This module provides the ability to start and manage linked Erlang-style peer nodes,
//! similar to Erlang/OTP's `peer` module (successor to the deprecated `slave` module).
//!
//! # Overview
//!
//! Peer nodes are separate OS processes running the Ambitious runtime that are started
//! and managed by an originating node. Key features:
//!
//! - **Automatic termination**: Peer nodes terminate when the control connection is lost
//! - **Linked lifecycle**: Use `start_link` to tie peer lifetime to the calling process
//! - **RPC**: Call functions and send messages to processes on peer nodes
//! - **Flexible spawning**: Spawn via direct execution, SSH, or custom commands
//!
//! # Use Cases
//!
//! - **Integration testing**: Start multiple nodes for distributed tests
//! - **Cluster management**: Programmatically scale a cluster
//! - **Fault tolerance testing**: Simulate node failures by stopping peers
//!
//! # Example
//!
//! ```ignore
//! use ambitious::peer::{Peer, PeerOptions};
//!
//! // Start a peer node
//! let peer = Peer::start(PeerOptions {
//!     name: Some("peer1".into()),
//!     args: vec!["--port".into(), "9001".into()],
//!     ..Default::default()
//! }).await?;
//!
//! // Wait for the peer to be ready
//! peer.wait_boot(Duration::from_secs(10)).await?;
//!
//! // Get the node name for distribution
//! let node = peer.node();
//!
//! // Stop the peer
//! peer.stop().await?;
//! ```

#![deny(warnings)]

mod core;
mod options;

pub use core::{Peer, PeerError, PeerState, random_name, random_name_with_prefix};
pub use options::{Connection, PeerOptions, ShutdownBehavior, SpawnMethod};
