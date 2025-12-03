# Starlang - Erlang-style Concurrency for Rust

A native Rust implementation of Erlang/OTP primitives, bringing the power of the BEAM's concurrency model to Rust with full type safety.

## Features

- **Processes**: Lightweight, isolated units of concurrency with mailboxes
- **Message Passing**: Type-safe send/receive between processes using the `Term` trait
- **Links & Monitors**: Bidirectional failure propagation and unidirectional observation
- **GenServer**: Request/response pattern with typed state management
- **Supervisor**: Fault-tolerant supervision trees with configurable restart strategies
- **DynamicSupervisor**: Start children on demand with automatic restart
- **Application**: OTP-style application lifecycle management
- **Distribution**: Connect Starlang nodes across the network with QUIC transport
- **Registry**: Local process registry with pub/sub support and via-tuple routing
- **Channels**: Phoenix-style channels for real-time communication
- **Presence**: Distributed presence tracking for real-time applications

## Quick Start

Add Starlang to your `Cargo.toml`:

```toml
[dependencies]
starlang = "0.1"
```

### Basic Process Spawning

```rust
use starlang::prelude::*;

#[starlang::main]
async fn main() {
    // Spawn a process
    let pid = starlang::spawn(|| async {
        println!("Hello from process {:?}", starlang::current_pid());
    });

    // Send a message
    starlang::send(pid, &"Hello!").ok();
}
```

### GenServer Example

```rust
use starlang::prelude::*;
use starlang::gen_server::{async_trait, GenServer, InitResult, CallResult, CastResult};
use serde::{Serialize, Deserialize};

struct Counter;

#[derive(Debug, Serialize, Deserialize)]
enum CounterCall {
    Get,
    Increment,
}

#[derive(Debug, Serialize, Deserialize)]
enum CounterCast {
    Reset,
}

#[async_trait]
impl GenServer for Counter {
    type State = i64;
    type InitArg = i64;
    type Call = CounterCall;
    type Cast = CounterCast;
    type Reply = i64;

    async fn init(initial: i64) -> InitResult<i64> {
        InitResult::ok(initial)
    }

    async fn handle_call(
        request: CounterCall,
        _from: From,
        state: &mut i64,
    ) -> CallResult<i64, i64> {
        match request {
            CounterCall::Get => CallResult::reply(*state, *state),
            CounterCall::Increment => {
                *state += 1;
                CallResult::reply(*state, *state)
            }
        }
    }

    async fn handle_cast(msg: CounterCast, _state: &mut i64) -> CastResult<i64> {
        match msg {
            CounterCast::Reset => CastResult::noreply(0),
        }
    }

    // ... handle_info and handle_continue implementations
}
```

### Supervisor Example

```rust
use starlang::prelude::*;
use starlang::supervisor::{Supervisor, SupervisorInit, SupervisorFlags, ChildSpec, Strategy};

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
                    Ok(starlang::spawn(|| async { /* worker code */ }))
                }),
            ],
        )
    }
}
```

### DynamicSupervisor Example

```rust
use starlang::supervisor::dynamic_supervisor::{self, DynamicSupervisorOpts};
use starlang::supervisor::ChildSpec;

// Start a dynamic supervisor
let sup_pid = dynamic_supervisor::start(DynamicSupervisorOpts::new()).await?;

// Start children on demand
let child_spec = ChildSpec::new("worker", || async {
    Ok(starlang::spawn(|| async { /* worker code */ }))
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
starlang/
├── starlang/              # Main crate re-exporting all functionality
├── starlang-core/         # Core types: Pid, Ref, ExitReason, Term trait
├── starlang-runtime/      # Async runtime, scheduler, process registry
├── starlang-process/      # Process spawning, mailboxes, links, monitors
├── starlang-gen-server/   # GenServer trait and implementation
├── starlang-supervisor/   # Supervisor trait and restart strategies
├── starlang-application/  # Application lifecycle management
├── starlang-macros/       # Procedural macros for ergonomic APIs
└── examples/
    └── chat/           # Distributed chat server example
```

## Core Concepts

### Term Trait

The `Term` trait (similar to Erlang's term concept) enables any serializable type to be sent between processes:

```rust
use starlang::Term;

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
starlang::register("my_server".to_string(), pid);

// Look up by name
if let Some(pid) = starlang::whereis("my_server") {
    starlang::send(pid, &message)?;
}
```

### Via Tuple (Custom Registries)

Similar to Elixir's `{:via, module, term}` pattern:

```rust
use starlang::gen_server::ServerRef;
use starlang::registry::Registry;

// Create a custom registry
let registry: Arc<Registry<Vec<u8>, ()>> = Arc::new(Registry::unique("my_registry"));

// Reference a server via the registry with any Term as key
let server_ref = ServerRef::via(registry, ("room", "lobby"));

// Use it with GenServer calls
let result = gen_server::call::<MyServer>(server_ref, request, timeout).await?;
```

## Distribution

Starlang supports distributed nodes connected via QUIC:

```rust
use starlang::node;

// Start distribution
node::start("node1@localhost", "127.0.0.1:9000").await?;

// Connect to another node
node::connect("node2@localhost", "127.0.0.1:9001").await?;

// Send messages to remote processes
starlang::send(remote_pid, &message)?;
```

## Building and Testing

```bash
# Build all crates
cargo build

# Run all tests
cargo test

# Run a specific crate's tests
cargo test -p starlang-gen-server

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
