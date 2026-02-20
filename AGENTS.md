# Ambitious - Distributed Rust Erlang Abstract Machine

A native Rust implementation of Erlang/OTP primitives, bringing the power of the BEAM's concurrency model to Rust with full type safety.

## Project Vision

Ambitious aims to provide Erlang-style concurrency primitives in Rust:
- **Processes**: Lightweight, isolated units of concurrency with mailboxes
- **Message Passing**: Type-safe send/receive between processes
- **Links & Monitors**: Bidirectional failure propagation and unidirectional observation
- **GenServer**: Request/response pattern with typed state management
- **Supervisor**: Fault-tolerant supervision trees with configurable restart strategies
- **Application**: OTP-style application lifecycle management

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                   Application Layer                         │
│  (Application trait, dependency management)                 │
└─────────────────────────────────────────────────────────────┘
                              ▲
┌─────────────────────────────────────────────────────────────┐
│              Supervision Layer                              │
│  (Supervisor, ChildSpec, restart strategies)                │
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

## Core Types

### Pid (Process Identifier)
```rust
pub struct Pid {
    node: u32,  // For future distribution support
    id: u64,    // Unique process ID
}
```

### Ref (Monitor/Timer Reference)
```rust
pub struct Ref(u64);  // Unique, atomically generated
```

### ExitReason
```rust
pub enum ExitReason {
    Normal,
    Shutdown,
    ShutdownReason(String),
    Killed,
    Error(String),
}
```

## Process API (mirrors Elixir's Process module)

```rust
// Spawning
fn spawn<F, T>(f: F) -> Pid;
fn spawn_link<F, T>(f: F) -> Pid;
fn spawn_monitor<F, T>(f: F) -> (Pid, Ref);

// Links (bidirectional)
fn link(pid: Pid) -> Result<(), SendError>;
fn unlink(pid: Pid);

// Monitors (unidirectional)
fn monitor(pid: Pid) -> Result<Ref, SendError>;
fn demonitor(reference: Ref);

// Messaging
fn send<M: Term>(pid: Pid, msg: &M) -> Result<(), SendError>;
fn send_after<M: Term>(pid: Pid, msg: &M, delay: Duration) -> TimerResult;

// Process flags
fn flag(flag: ProcessFlag, value: bool) -> bool;  // trap_exit, etc.

// Exit signals
fn exit(pid: Pid, reason: ExitReason);

// Info
fn alive(pid: Pid) -> bool;
fn current_pid() -> Pid;

// Registration
fn register(name: String, pid: Pid);
fn whereis(name: &str) -> Option<Pid>;
fn unregister(name: &str);
```

## GenServer API (mirrors Elixir's GenServer)

### Trait Definition
```rust
#[async_trait]
pub trait GenServer: Sized + Send + 'static {
    type Args: Send + 'static;
    type Call: Message + Send + 'static;
    type Cast: Message + Send + 'static;
    type Info: Message + Send + 'static;
    type Reply: Message + Send + 'static;

    async fn init(args: Self::Args) -> Init<Self>;
    async fn handle_call(&mut self, msg: Self::Call, from: From) -> Reply<Self::Reply>;
    async fn handle_cast(&mut self, msg: Self::Cast) -> Status;
    async fn handle_info(&mut self, msg: Self::Info) -> Status;
    async fn terminate(&mut self, _reason: ExitReason) {}
    async fn handle_timeout(&mut self) -> Status { Status::Ok }
    async fn handle_continue(&mut self, _arg: Vec<u8>) -> Status { Status::Ok }
}
```

Key design points:
- The struct IS the state (no separate `State` type)
- Handlers take `&mut self`
- `init` returns `Init<Self>`
- `handle_call` returns `Reply<Self::Reply>`
- `handle_cast`/`handle_info` return `Status`

### Return Types
```rust
pub enum Init<S> {
    Ok(S),
    Continue(S, Vec<u8>),
    Timeout(S, Duration),
    Ignore,
    Stop(ExitReason),
}

pub enum Reply<T> {
    Ok(T),
    Continue(T, Vec<u8>),
    Timeout(T, Duration),
    NoReply,
    Stop(ExitReason, T),
    StopNoReply(ExitReason),
}

pub enum Status {
    Ok,
    Continue(Vec<u8>),
    Timeout(Duration),
    Stop(ExitReason),
}
```

### Client Functions
```rust
// Starting
async fn start<G: GenServer>(args: G::Args) -> Result<Pid, Error>;
async fn start_link<G: GenServer>(args: G::Args) -> Result<Pid, Error>;

// Messaging
async fn call<G, R>(server: impl Into<ServerRef>, msg: G::Call, timeout: Duration) -> Result<R, Error>;
fn cast<G>(server: impl Into<ServerRef>, msg: G::Cast);

// Deferred reply (for NoReply pattern)
fn reply<T: Message>(from: &From, value: T);

// Graceful shutdown
async fn stop(server: impl Into<ServerRef>, reason: ExitReason) -> Result<(), Error>;
```

## Supervisor API (mirrors Elixir's Supervisor)

### Strategies
```rust
pub enum Strategy {
    OneForOne,   // Restart only failed child
    OneForAll,   // Restart all on any failure
    RestForOne,  // Restart failed + children started after
}
```

### Child Specification
```rust
pub struct ChildSpec {
    pub id: String,
    pub start: StartFn,           // Function to start child
    pub restart: RestartType,     // Permanent, Transient, Temporary
    pub shutdown: ShutdownType,   // BrutalKill, Timeout(ms), Infinity
    pub child_type: ChildType,    // Worker or Supervisor
}

pub enum RestartType {
    Permanent,  // Always restart
    Transient,  // Restart on abnormal exit
    Temporary,  // Never restart
}

pub enum ShutdownType {
    BrutalKill,
    Timeout(Duration),
    Infinity,
}
```

### Supervisor Trait
```rust
pub trait Supervisor: Sized + Send + 'static {
    type InitArg: Send + 'static;

    fn init(arg: Self::InitArg) -> SupervisorInit;
}

pub struct SupervisorInit {
    pub flags: SupervisorFlags,
    pub children: Vec<ChildSpec>,
}
```

### Client Functions
```rust
async fn start_link<S: Supervisor>(handle: &RuntimeHandle, parent: Pid, arg: S::InitArg) -> Result<Pid, StartError>;
async fn start<S: Supervisor>(handle: &RuntimeHandle, arg: S::InitArg) -> Result<Pid, StartError>;
async fn terminate_child(handle: &RuntimeHandle, sup: Pid, id: &str) -> Result<(), TerminateError>;
async fn delete_child(handle: &RuntimeHandle, sup: Pid, id: &str) -> Result<(), DeleteError>;
async fn restart_child(handle: &RuntimeHandle, sup: Pid, id: &str) -> Result<Pid, RestartError>;
async fn which_children(handle: &RuntimeHandle, sup: Pid) -> Result<Vec<ChildInfo>, String>;
async fn count_children(handle: &RuntimeHandle, sup: Pid) -> Result<ChildCounts, String>;
```

## Term Trait

The `Term` trait provides serialization for any type sent between processes:

```rust
pub trait Term: Sized + Send + 'static {
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError>;
    fn try_encode(&self) -> Option<Vec<u8>>;
}

// Blanket implementation for all Serialize + DeserializeOwned types
impl<T: Serialize + DeserializeOwned + Send + 'static> Term for T { ... }
```

## Message Trait

The `Message` trait provides typed, self-describing messages with tag-based dispatch:

```rust
pub trait Message: Sized + Send + 'static {
    const TAG: &'static str;
    fn encode_local(&self) -> Vec<u8>;
    fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError>;
    fn encode_remote(&self) -> Vec<u8>;
    fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError>;
}
```

Use `#[derive(Message)]` from `ambitious-macros` for automatic implementation.

## Macros

### Process Macros
```rust
// Receive with pattern matching
receive! {
    msg: MyMessage => { ... },
    SystemMessage::Exit { from, reason } => { ... },
    after Duration::from_secs(5) => { ... },
}

// Spawn with automatic linking
spawn_link!(my_function);
spawn_link!(MyModule::start, args);
```

### Derive Macros
```rust
#[derive(Message)]
struct Get;

#[derive(Message)]
enum CounterCall {
    Get,
    Increment,
}
```

## Development Guidelines

- Use `tokio` as the async runtime
- Prefer `postcard` for message serialization (compact, fast)
- Use `dashmap` for concurrent process registry
- All public APIs should mirror Elixir naming where possible
- Extensive use of type parameters for compile-time safety
- Macros for ergonomic APIs that feel Elixir-like

## Testing

Each module should have:
- Unit tests for core functionality
- Integration tests for cross-process behavior
- Doc tests for public APIs
- Examples demonstrating common patterns

## Build Commands

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests
cargo test -p ambitious        # Test the main crate
cargo doc --open               # Generate and view docs
```

## References

- [Elixir GenServer](https://hexdocs.pm/elixir/GenServer.html)
- [Elixir Supervisor](https://hexdocs.pm/elixir/Supervisor.html)
- [Elixir Process](https://hexdocs.pm/elixir/Process.html)
- [Erlang gen_server](https://www.erlang.org/doc/man/gen_server.html)
- Rebirth project: ../rebirth (WASM-based inspiration)

# Remember

- always run `just ci` before committing
