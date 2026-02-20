<p align="center">
  <img src="https://raw.githubusercontent.com/scrogson/ambitious/main/assets/ambitious.png" alt="Ambitious Logo" width="200">
</p>

# Ambitious - Erlang-style Concurrency for Rust

A native Rust implementation of Erlang/OTP primitives, bringing the power of the BEAM's concurrency model to Rust with full type safety.

## Features

- **Processes**: Lightweight, isolated units of concurrency with mailboxes
- **Message Passing**: Type-safe send/receive between processes using the `Term` trait
- **Links & Monitors**: Bidirectional failure propagation and unidirectional observation
- **GenServer**: Request/response pattern with typed state management
- **Supervisor**: Fault-tolerant supervision trees with configurable restart strategies
- **DynamicSupervisor**: Start children on demand with automatic restart
- **Application**: OTP-style application lifecycle management
- **Distribution**: Connect Ambitious nodes across the network with QUIC transport
- **Registry**: Local process registry with pub/sub support and via-tuple routing
- **Channels**: Phoenix-style channels for real-time communication
- **Presence**: Distributed presence tracking for real-time applications

## Quick Start

Add Ambitious to your `Cargo.toml`:

```toml
[dependencies]
ambitious = "0.1"
```

### Basic Process Spawning

```rust
use ambitious::prelude::*;

#[ambitious::main]
async fn main() {
    // Spawn a process
    let pid = ambitious::spawn(|| async {
        println!("Hello from process {:?}", ambitious::current_pid());
    });

    // Send a message
    ambitious::send(pid, &"Hello!").ok();
}
```

### GenServer Example

```rust
use ambitious::prelude::*;
use ambitious::gen_server::{self, async_trait, GenServer};
use serde::{Serialize, Deserialize};

struct Counter {
    count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
enum CounterCall {
    Get,
    Increment,
}

#[derive(Debug, Serialize, Deserialize)]
enum CounterCast {
    Reset,
}

#[derive(Debug, Serialize, Deserialize)]
struct CounterReply(i64);

// Message trait implementations omitted for brevity.
// Use #[derive(Message)] from ambitious-macros for automatic implementation.

#[async_trait]
impl GenServer for Counter {
    type Args = i64;
    type Call = CounterCall;
    type Cast = CounterCast;
    type Info = ();
    type Reply = CounterReply;

    async fn init(initial: i64) -> Init<Self> {
        Init::Ok(Counter { count: initial })
    }

    async fn handle_call(&mut self, msg: CounterCall, _from: From) -> Reply<CounterReply> {
        match msg {
            CounterCall::Get => Reply::Ok(CounterReply(self.count)),
            CounterCall::Increment => {
                self.count += 1;
                Reply::Ok(CounterReply(self.count))
            }
        }
    }

    async fn handle_cast(&mut self, msg: CounterCast) -> Status {
        match msg {
            CounterCast::Reset => {
                self.count = 0;
                Status::Ok
            }
        }
    }

    async fn handle_info(&mut self, _msg: ()) -> Status {
        Status::Ok
    }
}
```

### Supervisor Example

```rust
use ambitious::prelude::*;
use ambitious::supervisor::{Supervisor, SupervisorInit, SupervisorFlags, ChildSpec, Strategy};

struct MySupervisor;

impl Supervisor for MySupervisor {
    type InitArg = ();

    fn init(_arg: ()) -> SupervisorInit {
        SupervisorInit::new(
            SupervisorFlags::new(Strategy::OneForOne)
                .max_restarts(3)
                .max_seconds(5),
            vec![
                ChildSpec::new("worker", || async {
                    // Start your worker here
                    Ok(ambitious::spawn(|| async { /* worker code */ }))
                }),
            ],
        )
    }
}
```

### DynamicSupervisor Example

```rust
use ambitious::supervisor::dynamic_supervisor::{self, DynamicSupervisorOpts};
use ambitious::supervisor::ChildSpec;

// Start a dynamic supervisor
let sup_pid = dynamic_supervisor::start(DynamicSupervisorOpts::new()).await?;

// Start children on demand
let child_spec = ChildSpec::new("worker", || async {
    Ok(ambitious::spawn(|| async { /* worker code */ }))
});
let child_pid = dynamic_supervisor::start_child(sup_pid, child_spec).await?;

// Terminate children
dynamic_supervisor::terminate_child(sup_pid, child_pid)?;
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                   Application Layer                         │
│  (Application trait, dependency management)                 │
└─────────────────────────────────────────────────────────────┘
                              ▲
┌─────────────────────────────────────────────────────────────┐
│              Supervision Layer                              │
│  (Supervisor, DynamicSupervisor, ChildSpec, strategies)     │
└─────────────────────────────────────────────────────────────┘
                              ▲
┌─────────────────────────────────────────────────────────────┐
│           GenServer Pattern Layer                           │
│  (GenServer trait, call/cast/info, typed messages)          │
└─────────────────────────────────────────────────────────────┘
                              ▲
┌─────────────────────────────────────────────────────────────┐
│         Process Base Layer                                  │
│  (Pid, spawn, link, monitor, send/receive, mailbox)         │
└─────────────────────────────────────────────────────────────┘
                              ▲
┌─────────────────────────────────────────────────────────────┐
│              Runtime Layer                                  │
│  (Scheduler, process registry, async executor)              │
└─────────────────────────────────────────────────────────────┘
```

## Crate Structure

```
ambitious/
├── crates/
│   ├── ambitious/              # Main crate containing all functionality
│   │   └── src/
│   │       ├── core/           # Core types: Pid, Ref, ExitReason, Atom, Term trait
│   │       ├── runtime/        # Async runtime, scheduler, process registry, task-locals
│   │       ├── process/        # Process spawning, mailboxes, links, monitors
│   │       ├── gen_server/     # GenServer trait and server loop
│   │       ├── supervisor/     # Supervisor trait and restart strategies
│   │       ├── application/    # Application lifecycle management
│   │       ├── timer/          # Timer management (send_after, cancel)
│   │       └── ...             # Registry, channels, distribution, etc.
│   └── ambitious-macros/       # Procedural macros (#[derive(Message)], #[ambitious::main])
```

## Core Concepts

### Term Trait

The `Term` trait (similar to Erlang's term concept) enables any serializable type to be sent between processes:

```rust
use ambitious::Term;

// Any type implementing Serialize + DeserializeOwned automatically implements Term
#[derive(Serialize, Deserialize)]
struct MyMessage {
    content: String,
}

// Encode and decode
let msg = MyMessage { content: "hello".into() };
let bytes = msg.encode();
let decoded: MyMessage = Term::decode(&bytes)?;
```

### Process Registration

```rust
// Register a process by name
ambitious::register("my_server".to_string(), pid);

// Look up by name
if let Some(pid) = ambitious::whereis("my_server") {
    ambitious::send(pid, &message)?;
}
```

### Via Tuple (Custom Registries)

Similar to Elixir's `{:via, module, term}` pattern:

```rust
use ambitious::gen_server::ServerRef;
use ambitious::registry::Registry;

// Create a custom registry
let registry: Arc<Registry<Vec<u8>, ()>> = Arc::new(Registry::unique("my_registry"));

// Reference a server via the registry with any Term as key
let server_ref = ServerRef::via(registry, ("room", "lobby"));

// Use it with GenServer calls
let result = gen_server::call::<MyServer>(server_ref, request, timeout).await?;
```

## Distribution

Ambitious supports distributed nodes connected via QUIC:

```rust
use ambitious::node;

// Start distribution
node::start("node1@localhost", "127.0.0.1:9000").await?;

// Connect to another node
node::connect("node2@localhost", "127.0.0.1:9001").await?;

// Send messages to remote processes
ambitious::send(remote_pid, &message)?;
```

## Building and Testing

```bash
# Build all crates
cargo build

# Run all tests
cargo test

# Run the main crate's tests
cargo test -p ambitious

# Run the chat example
cargo run --example chat-server
```

## References

- [Elixir GenServer](https://hexdocs.pm/elixir/GenServer.html)
- [Elixir Supervisor](https://hexdocs.pm/elixir/Supervisor.html)
- [Elixir DynamicSupervisor](https://hexdocs.pm/elixir/DynamicSupervisor.html)
- [Elixir Process](https://hexdocs.pm/elixir/Process.html)
- [Elixir Registry](https://hexdocs.pm/elixir/Registry.html)
- [Erlang gen_server](https://www.erlang.org/doc/man/gen_server.html)

## License

MIT
