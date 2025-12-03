# Starlang Chat Server

A multi-user chat application demonstrating Starlang's capabilities:

- **Processes** for user sessions (one per TCP connection)
- **GenServers** for rooms and the room registry
- **Message passing** between processes
- **Concurrent connection handling**

## Architecture

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

## Running

### Start the server

```bash
cargo run --bin chat-server
```

The server listens on `127.0.0.1:9999` by default.

### Connect with the client

```bash
cargo run --bin chat-client
```

Or connect with netcat (note: uses binary protocol, so netcat won't work well):

```bash
nc localhost 9999
```

## Commands

| Command | Description |
|---------|-------------|
| `/nick <name>` | Set your nickname |
| `/join <room>` | Join a room (creates it if it doesn't exist) |
| `/leave <room>` | Leave a room |
| `/msg <room> <text>` | Send a message to a room |
| `/rooms` | List all rooms |
| `/users <room>` | List users in a room |
| `/quit` | Disconnect |

Short forms: `/n`, `/j`, `/l`, `/m`, `/r`, `/u`, `/q`

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

## Example Session

```
$ cargo run --bin chat-client
Connecting to 127.0.0.1:9999...
Connected! Type /help for commands.

Welcome to Starlang Chat! Use /nick <name> to set your nickname.

> /nick alice
[Nickname set to 'alice']
> /join general
[Joined #general]
> /msg general Hello everyone!
[#general] <alice> Hello everyone!
> /users general
[Users in #general:]
  alice
> /quit
[Goodbye!]
```

## Starlang Concepts Demonstrated

1. **Processes**: Each client connection spawns a session process
2. **GenServer**: Rooms and registry are GenServers with call/cast semantics
3. **Message Passing**: Sessions communicate with rooms via messages
4. **Concurrent Handling**: Multiple clients handled simultaneously
5. **Process Isolation**: Each session is isolated; one crash doesn't affect others
