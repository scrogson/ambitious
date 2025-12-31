# Ambitious Chat Server

A fully distributed multi-user chat application demonstrating Ambitious's capabilities:

- **Processes** for user sessions (one per TCP connection)
- **GenServers** for rooms and the room registry
- **DynamicSupervisor** for managing room processes
- **Channels** - Phoenix-style topic-based real-time communication
- **Presence** - distributed presence tracking with custom metadata
- **Distribution** - connect multiple chat servers together
- **Global Registry** - rooms are registered globally across all nodes
- **pg (Process Groups)** - distributed process groups for room membership
- **Cross-node messaging** - users on different nodes can chat in the same room

## Architecture

### Single Node

```
┌─────────────────────────────────────────────────────────────┐
│                      TCP Acceptor                           │
│                    (accepts connections)                    │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Session Processes                        │
│              (one per connected client)                     │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐       │
│  │ Session │  │ Session │  │ Session │  │ Session │  ...  │
│  └─────────┘  └─────────┘  └─────────┘  └─────────┘       │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   Registry GenServer                        │
│              (manages room creation/lookup)                 │
│           Uses Global Registry for cross-node access        │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                  DynamicSupervisor                          │
│              (supervises room GenServers)                   │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Room GenServers                          │
│               (one per active room)                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                  │
│  │ #general │  │ #random  │  │ #tech    │  ...             │
│  └──────────┘  └──────────┘  └──────────┘                  │
└─────────────────────────────────────────────────────────────┘
```

### Distributed Cluster

```
┌─────────────────────────────────┐     ┌─────────────────────────────────┐
│           Node 1                │     │           Node 2                │
│         (node1@localhost)       │     │         (node2@localhost)       │
│                                 │     │                                 │
│  ┌─────────────────────────┐   │     │   ┌─────────────────────────┐  │
│  │    TCP :9999            │   │     │   │    TCP :9998            │  │
│  │  (client connections)   │   │     │   │  (client connections)   │  │
│  └─────────────────────────┘   │     │   └─────────────────────────┘  │
│             │                   │     │             │                  │
│  ┌──────────┴──────────┐       │     │       ┌─────┴──────────┐       │
│  │ alice   │   bob     │       │     │       │ charlie        │       │
│  │ Session │   Session │       │     │       │ Session        │       │
│  └─────────┴───────────┘       │     │       └────────────────┘       │
│             │                   │     │             │                  │
│  ┌──────────┴──────────┐       │     │       ┌─────┴──────────┐       │
│  │   Registry          │◄──────┼─────┼──────►│   Registry     │       │
│  │ (Global Registry)   │       │     │       │ (Global Reg.)  │       │
│  └─────────────────────┘       │     │       └────────────────┘       │
│             │                   │     │             │                  │
│  ┌──────────┴──────────┐       │     │             │                  │
│  │   #general Room     │◄──────┼─────┼─────────────┘                  │
│  │   (pg group)        │       │     │   (charlie joins #general      │
│  └─────────────────────┘       │     │    via global registry)        │
│                                 │     │                                 │
│    Distribution Port :9000     │◄───►│    Distribution Port :9001     │
│         (QUIC)                 │     │         (QUIC)                 │
└─────────────────────────────────┘     └─────────────────────────────────┘
                    │                               │
                    └───────────────────────────────┘
                           QUIC Distribution
                        (encrypted, multiplexed)
```

## Running

### Single Node

```bash
cargo run --bin chat-server
```

The server listens on `127.0.0.1:9999` by default.

### Distributed Cluster

Start the first node:

```bash
cargo run --bin chat-server -- --name node1 --port 9999 --dist-port 9000
```

Start a second node and connect to the first:

```bash
cargo run --bin chat-server -- --name node2 --port 9998 --dist-port 9001 --connect 127.0.0.1:9000
```

Now clients connecting to either node can chat with each other!

### Connect with the TUI Client

```bash
cargo run --bin chat-client
# Or connect to the second node:
cargo run --bin chat-client -- --port 9998
```

The client provides a full terminal UI with:
- Room list sidebar
- Message history with timestamps
- User list for the current room
- Input area with command history
- Configurable themes via `~/.config/ambitious-chat/config.toml`

## Commands

| Command | Short | Description |
|---------|-------|-------------|
| `/nick <name>` | `/n` | Set your nickname |
| `/join <room>` | `/j` | Join a room (creates it if it doesn't exist) |
| `/leave` | `/l` | Leave current room |
| `/rooms` | `/r` | List all rooms (across all nodes) |
| `/users` | `/u` | List users in current room |
| `/quit` | `/q` | Disconnect |
| `/help` | `/h` | Show help |

Just type a message (without `/`) to send it to the current room.

## Distribution Features

### Global Registry

Rooms are registered in the global registry, making them accessible from any node:

```rust
// Register a room globally
ambitious::dist::global::register("room:general", room_pid);

// Look up a room from any node
if let Some(pid) = ambitious::dist::global::whereis("room:general") {
    // Can send messages to the room, even if it's on another node
}
```

### Process Groups (pg)

Room membership uses distributed process groups:

```rust
// Join a room's process group
ambitious::dist::pg::join("room:general:members", session_pid);

// Get all members across all nodes
let members = ambitious::dist::pg::get_members("room:general:members");
```

### Cross-Node Messaging

When a user sends a message:
1. The message goes to the room GenServer (may be on another node)
2. The room looks up all members via pg (distributed)
3. Each member's session receives the message
4. Sessions send the message to their TCP clients

### Channels (Phoenix-style)

Rooms use Phoenix-style Channels for topic-based communication:

```rust
use ambitious::channel::{Channel, Socket, JoinResult, HandleResult};

struct RoomChannel;

impl Channel for RoomChannel {
    type Assigns = RoomAssigns;       // Custom socket state
    type JoinPayload = JoinPayload;   // Join request data
    type InEvent = RoomInEvent;       // Client → Server events
    type OutEvent = RoomOutEvent;     // Server → Client events

    fn topic_pattern() -> &'static str {
        "room:*"  // Matches "room:general", "room:lobby", etc.
    }

    async fn join(topic: &str, payload: JoinPayload, socket: Socket<()>) -> JoinResult<RoomAssigns> {
        // Authorize join, set up assigns
        let room_name = topic.strip_prefix("room:").unwrap();
        JoinResult::Ok(socket.assign(RoomAssigns { nick: payload.nick, room_name }))
    }

    async fn handle_in(event: &str, payload: InEvent, socket: &mut Socket<Assigns>) -> HandleResult<OutEvent> {
        match event {
            "new_msg" => HandleResult::Broadcast {
                event: "new_msg".to_string(),
                payload: RoomOutEvent::Message { from: socket.assigns.nick.clone(), text },
            },
            _ => HandleResult::NoReply,
        }
    }
}
```

### Presence (Real-time User Tracking)

Track users across the distributed cluster with custom metadata:

```rust
use ambitious::presence;

#[derive(Clone, Serialize, Deserialize)]
struct UserMeta {
    nick: String,
    status: String,  // "online", "away", "typing"
}

// Track a user's presence in a room
presence::track("room:general", "user:123", UserMeta {
    nick: "alice".into(),
    status: "online".into(),
});

// Update presence (e.g., user starts typing)
presence::update("room:general", "user:123", UserMeta {
    nick: "alice".into(),
    status: "typing".into(),
});

// List all presences in a topic (works across nodes!)
let presences = presence::list("room:general");
for (key, metas) in presences {
    println!("{}: {:?}", key, metas);
}

// Untrack when user leaves
presence::untrack("room:general", "user:123");
```

Presence automatically syncs across all nodes using a CRDT-based approach - no single point of failure!

## Protocol

The chat protocol uses length-prefixed binary messages serialized with postcard.

### Message Format

```
┌────────────────┬────────────────────────────────┐
│ Length (4 BE)  │ Payload (postcard-serialized)  │
└────────────────┴────────────────────────────────┘
```

### Client Commands

```rust
enum ClientCommand {
    Nick(String),
    Join(String),
    Leave(String),
    Msg { room: String, text: String },
    ListRooms,
    ListUsers(String),
    Quit,
}
```

### Server Events

```rust
enum ServerEvent {
    Welcome { message: String },
    NickOk { nick: String },
    NickError { reason: String },
    Joined { room: String },
    JoinError { room: String, reason: String },
    Left { room: String },
    Message { room: String, from: String, text: String },
    UserJoined { room: String, nick: String },
    UserLeft { room: String, nick: String },
    RoomList { rooms: Vec<RoomInfo> },
    UserList { room: String, users: Vec<String> },
    Error { message: String },
    Shutdown,
}
```

## Example: Cross-Node Chat Session

Terminal 1 - Start node1:
```bash
$ cargo run --bin chat-server -- --name node1 --port 9999 --dist-port 9000
INFO Starting Ambitious Chat Server name=node1 port=9999 dist_port=9000
INFO Distribution started node=node1@localhost addr=0.0.0.0:9000
```

Terminal 2 - Start node2 and connect to node1:
```bash
$ cargo run --bin chat-server -- --name node2 --port 9998 --dist-port 9001 --connect 127.0.0.1:9000
INFO Starting Ambitious Chat Server name=node2 port=9999 dist_port=9001
INFO Distribution started node=node2@localhost addr=0.0.0.0:9001
INFO Connected to peer node peer=127.0.0.1:9000 node_id=node1@localhost
```

Terminal 3 - Alice connects to node1:
```bash
$ cargo run --bin chat-client -- --port 9999
> /nick alice
> /join general
> hello from node1!
```

Terminal 4 - Bob connects to node2:
```bash
$ cargo run --bin chat-client -- --port 9998
> /nick bob
> /join general      # Joins the same room, created on node1
[#general] <alice> hello from node1!
> hello from node2!
```

Alice sees Bob's message, even though they're connected to different servers!

## Ambitious Concepts Demonstrated

1. **Processes**: Each client connection spawns a session process
2. **GenServer**: Rooms and registry are GenServers with call/cast semantics
3. **DynamicSupervisor**: Room processes are dynamically supervised
4. **Message Passing**: Sessions communicate with rooms via messages
5. **Concurrent Handling**: Multiple clients handled simultaneously
6. **Process Isolation**: Each session is isolated; one crash doesn't affect others
7. **Distribution**: Multiple nodes form a cluster with QUIC transport
8. **Global Registry**: Named processes accessible from any node
9. **Process Groups (pg)**: Distributed group membership for pub/sub patterns
10. **Channels**: Phoenix-style topic routing with join/handle_in/broadcast
11. **Presence**: CRDT-based distributed presence tracking with custom metadata
