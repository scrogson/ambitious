//! Integration tests for distributed chat.
//!
//! Tests that rooms work correctly across multiple nodes.
//! Uses the `starlang::peer` module for managing chat server peers.

use serde::{Deserialize, Serialize};
use starlang::peer::{Peer, PeerOptions, SpawnMethod};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Configuration for a chat server peer.
struct ChatServerConfig {
    name: String,
    port: u16,
    dist_port: u16,
    ws_port: u16,
    http_port: u16,
    connect: Option<String>,
}

impl ChatServerConfig {
    fn new(name: &str, port: u16, dist_port: u16, ws_port: u16, http_port: u16) -> Self {
        Self {
            name: name.to_string(),
            port,
            dist_port,
            ws_port,
            http_port,
            connect: None,
        }
    }

    fn connect_to(mut self, peer_addr: &str) -> Self {
        self.connect = Some(peer_addr.to_string());
        self
    }
}

/// Start a chat server peer using the peer module.
async fn start_chat_peer(config: ChatServerConfig) -> Peer {
    // Use the debug binary (built by cargo test)
    // CARGO_MANIFEST_DIR points to examples/chat, so we go up two levels to workspace root
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let binary = workspace_root.join("target/debug/chat-server");

    let mut options = PeerOptions::new()
        .name(&config.name)
        .spawn_method(SpawnMethod::Executable(binary))
        .dist_port(config.dist_port)
        .port("port", config.port)
        .port("ws-port", config.ws_port)
        .port("http-port", config.http_port)
        .env(
            "RUST_LOG",
            "info,starlang::distribution=debug,chat_server::session=debug,chat_server::channel=debug",
        )
        .boot_timeout(Duration::from_secs(10));

    if let Some(peer_addr) = config.connect {
        options = options.arg("--connect").arg(peer_addr);
    }

    Peer::start_link(options)
        .await
        .expect("Failed to start chat server peer")
}

/// Helper to connect a client and return the stream.
async fn connect_client(port: u16) -> TcpStream {
    let mut attempts = 0;
    loop {
        match TcpStream::connect(format!("127.0.0.1:{}", port)).await {
            Ok(stream) => return stream,
            Err(_) if attempts < 100 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!(
                "Failed to connect to server on port {} after 10s: {}",
                port, e
            ),
        }
    }
}

/// Send a framed message to the server.
async fn send_message(stream: &mut TcpStream, msg: &[u8]) {
    let len = msg.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.unwrap();
    stream.write_all(msg).await.unwrap();
    stream.flush().await.unwrap();
}

/// Read a framed message from the server.
async fn read_message(stream: &mut TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.unwrap();
    buf
}

/// Encode a client command using postcard.
fn encode_command(cmd: &ClientCommand) -> Vec<u8> {
    postcard::to_allocvec(cmd).unwrap()
}

/// Decode a server event using postcard.
fn decode_event(data: &[u8]) -> ServerEvent {
    postcard::from_bytes(data).unwrap()
}

// Re-define the protocol types here for testing (must match src/protocol.rs exactly)

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ClientCommand {
    Nick(String),
    Join(String),
    Leave(String),
    Msg { room: String, text: String },
    ListRooms,
    ListUsers(String),
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ServerEvent {
    Welcome {
        message: String,
    },
    NickOk {
        nick: String,
    },
    NickError {
        reason: String,
    },
    Joined {
        room: String,
    },
    JoinError {
        room: String,
        reason: String,
    },
    Left {
        room: String,
    },
    Message {
        room: String,
        from: String,
        text: String,
    },
    UserJoined {
        room: String,
        nick: String,
    },
    UserLeft {
        room: String,
        nick: String,
    },
    RoomList {
        rooms: Vec<RoomInfo>,
    },
    UserList {
        room: String,
        users: Vec<String>,
    },
    Error {
        message: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomInfo {
    name: String,
    user_count: usize,
}

#[tokio::test]
async fn test_single_node_chat() {
    // Start a single server using peer module
    let config = ChatServerConfig::new("node1", 19999, 19000, 14000, 18080);
    let _server = start_chat_peer(config).await;

    // Give server time to start
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Connect two clients
    let mut alice = connect_client(19999).await;
    let mut bob = connect_client(19999).await;

    // Read welcome messages
    let _ = read_message(&mut alice).await;
    let _ = read_message(&mut bob).await;

    // Alice sets nick
    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Nick("alice".into())),
    )
    .await;
    let resp = decode_event(&read_message(&mut alice).await);
    assert!(
        matches!(resp, ServerEvent::NickOk { ref nick } if nick == "alice"),
        "Expected NickOk, got {:?}",
        resp
    );

    // Bob sets nick
    send_message(
        &mut bob,
        &encode_command(&ClientCommand::Nick("bob".into())),
    )
    .await;
    let resp = decode_event(&read_message(&mut bob).await);
    assert!(
        matches!(resp, ServerEvent::NickOk { ref nick } if nick == "bob"),
        "Expected NickOk, got {:?}",
        resp
    );

    // Alice joins room
    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Join("testroom".into())),
    )
    .await;
    let resp = decode_event(&read_message(&mut alice).await);
    assert!(
        matches!(resp, ServerEvent::Joined { ref room } if room == "testroom"),
        "Expected Joined, got {:?}",
        resp
    );

    // Bob joins same room
    send_message(
        &mut bob,
        &encode_command(&ClientCommand::Join("testroom".into())),
    )
    .await;
    let resp = decode_event(&read_message(&mut bob).await);
    assert!(
        matches!(resp, ServerEvent::Joined { ref room } if room == "testroom"),
        "Expected Joined, got {:?}",
        resp
    );

    // Alice should get notification that Bob joined
    let resp = decode_event(&read_message(&mut alice).await);
    assert!(
        matches!(resp, ServerEvent::UserJoined { ref nick, .. } if nick == "bob"),
        "Expected UserJoined, got {:?}",
        resp
    );

    // Alice sends a message
    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Msg {
            room: "testroom".into(),
            text: "hello from alice".into(),
        }),
    )
    .await;

    // Bob should receive the message (may receive UserJoined/UserList first due to presence sync)
    let mut message_received = false;
    for _ in 0..5 {
        let resp = timeout(Duration::from_secs(2), read_message(&mut bob)).await;
        if resp.is_err() {
            break;
        }
        let event = decode_event(&resp.unwrap());
        match event {
            ServerEvent::Message {
                ref from, ref text, ..
            } if from == "alice" && text == "hello from alice" => {
                message_received = true;
                break;
            }
            ServerEvent::UserJoined { .. } | ServerEvent::UserList { .. } => {
                // Skip presence-related events
                continue;
            }
            other => {
                panic!("Expected Message or presence event, got {:?}", other);
            }
        }
    }
    assert!(message_received, "Bob should have received alice's message");

    // Server is automatically cleaned up when _server goes out of scope (start_link)
}

#[tokio::test]
async fn test_cross_node_global_registry() {
    // Start node1 using peer module
    let config1 = ChatServerConfig::new("node1", 29999, 29000, 24000, 28080);
    let _node1 = start_chat_peer(config1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Start node2 and connect to node1
    let config2 =
        ChatServerConfig::new("node2", 29998, 29001, 24001, 28081).connect_to("127.0.0.1:29000");
    let _node2 = start_chat_peer(config2).await;
    tokio::time::sleep(Duration::from_secs(2)).await; // Extra time for distribution handshake

    // Connect alice to node1
    let mut alice = connect_client(29999).await;
    let _ = read_message(&mut alice).await; // welcome

    // Alice sets nick and joins room
    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Nick("alice".into())),
    )
    .await;
    let _ = read_message(&mut alice).await;

    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Join("crossroom".into())),
    )
    .await;
    let resp = decode_event(&read_message(&mut alice).await);
    assert!(
        matches!(resp, ServerEvent::Joined { ref room } if room == "crossroom"),
        "Expected Joined, got {:?}",
        resp
    );

    // Give time for global registry to sync and drain any pending messages (like UserList from PushPresenceState)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Drain any pending messages from Alice (like UserList sent after her own join)
    while let Ok(msg) = timeout(Duration::from_millis(100), read_message(&mut alice)).await {
        let event = decode_event(&msg);
        tracing::debug!("Drained pending message from Alice: {:?}", event);
    }

    // Connect bob to node2
    let mut bob = connect_client(29998).await;
    let _ = read_message(&mut bob).await; // welcome

    // Bob sets nick
    send_message(
        &mut bob,
        &encode_command(&ClientCommand::Nick("bob".into())),
    )
    .await;
    let _ = read_message(&mut bob).await;

    // Bob joins the same room (should find it via global registry)
    send_message(
        &mut bob,
        &encode_command(&ClientCommand::Join("crossroom".into())),
    )
    .await;
    let resp = decode_event(&read_message(&mut bob).await);
    assert!(
        matches!(resp, ServerEvent::Joined { ref room } if room == "crossroom"),
        "Expected Joined, got {:?}",
        resp
    );

    // Alice should receive notification that Bob joined (via distribution)
    let resp = timeout(Duration::from_secs(3), read_message(&mut alice)).await;
    assert!(
        resp.is_ok(),
        "Alice should receive notification that Bob joined"
    );
    let resp = decode_event(&resp.unwrap());
    assert!(
        matches!(resp, ServerEvent::UserJoined { ref nick, .. } if nick == "bob"),
        "Expected UserJoined for bob, got {:?}",
        resp
    );

    // Bob sends a message
    send_message(
        &mut bob,
        &encode_command(&ClientCommand::Msg {
            room: "crossroom".into(),
            text: "hello from bob on node2".into(),
        }),
    )
    .await;

    // Alice should receive the message (routed across nodes)
    let resp = timeout(Duration::from_secs(3), read_message(&mut alice)).await;
    assert!(
        resp.is_ok(),
        "Alice should receive bob's message across nodes"
    );
    let resp = decode_event(&resp.unwrap());
    assert!(
        matches!(resp, ServerEvent::Message { ref from, ref text, .. } if from == "bob" && text == "hello from bob on node2"),
        "Expected message from bob, got {:?}",
        resp
    );

    // Servers are automatically cleaned up when _node1 and _node2 go out of scope
}

#[tokio::test]
async fn test_room_list_across_nodes() {
    // Start node1 using peer module
    let config1 = ChatServerConfig::new("node1", 39999, 39000, 34000, 38080);
    let _node1 = start_chat_peer(config1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Start node2 connected to node1
    let config2 =
        ChatServerConfig::new("node2", 39998, 39001, 34001, 38081).connect_to("127.0.0.1:39000");
    let _node2 = start_chat_peer(config2).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Connect alice to node1 and create a room
    let mut alice = connect_client(39999).await;
    let _ = read_message(&mut alice).await;
    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Nick("alice".into())),
    )
    .await;
    let _ = read_message(&mut alice).await;
    send_message(
        &mut alice,
        &encode_command(&ClientCommand::Join("room_on_node1".into())),
    )
    .await;
    let _ = read_message(&mut alice).await;

    // Give time for sync
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Connect bob to node2 and list rooms
    let mut bob = connect_client(39998).await;
    let _ = read_message(&mut bob).await;
    send_message(
        &mut bob,
        &encode_command(&ClientCommand::Nick("bob".into())),
    )
    .await;
    let _ = read_message(&mut bob).await;

    // List rooms from node2 - should see room created on node1
    send_message(&mut bob, &encode_command(&ClientCommand::ListRooms)).await;
    let resp = decode_event(&read_message(&mut bob).await);

    match resp {
        ServerEvent::RoomList { rooms } => {
            let room_names: Vec<_> = rooms.iter().map(|r| r.name.as_str()).collect();
            assert!(
                room_names.contains(&"room_on_node1"),
                "Node2 should see room created on node1, got: {:?}",
                room_names
            );
        }
        other => panic!("Expected RoomList event, got {:?}", other),
    }

    // Servers are automatically cleaned up when _node1 and _node2 go out of scope
}
