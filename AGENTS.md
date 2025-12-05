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
dream/
├── dream/              # Main crate re-exporting all functionality
├── ambitious-core/         # Core types: Pid, Ref, ExitReason, Message trait
├── ambitious-runtime/      # Async runtime, scheduler, process registry
├── ambitious-process/      # Process spawning, mailboxes, links, monitors
├── ambitious-gen-server/   # GenServer trait and implementation
├── ambitious-supervisor/   # Supervisor trait and restart strategies
├── ambitious-application/  # Application lifecycle management
└── ambitious-macros/       # Procedural macros for ergonomic APIs
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
fn link(pid: Pid) -> Result<(), ProcessError>;
fn unlink(pid: Pid);

// Monitors (unidirectional)
fn monitor(pid: Pid) -> Ref;
fn demonitor(reference: Ref);

// Messaging
fn send<M: Message>(pid: Pid, msg: M);
fn send_after<M: Message>(pid: Pid, msg: M, delay: Duration) -> TimerRef;

// Process flags
fn flag(flag: ProcessFlag, value: bool) -> bool;  // trap_exit, etc.

// Exit signals
fn exit(pid: Pid, reason: ExitReason);

// Info
fn alive(pid: Pid) -> bool;
fn self_pid() -> Pid;

// Registration
fn register(name: &str, pid: Pid) -> Result<(), RegistrationError>;
fn whereis(name: &str) -> Option<Pid>;
fn unregister(name: &str);
```

## GenServer API (mirrors Elixir's GenServer)

### Trait Definition
```rust
pub trait GenServer: Sized + Send + 'static {
    type State: Send;
    type InitArg: Send;
    type Call: Message;      // Request type
    type Cast: Message;      // Async message type
    type Reply: Message;     // Response type
    type Info: Message;      // Other messages (optional, default to bytes)

    // Callbacks
    fn init(arg: Self::InitArg) -> InitResult<Self::State>;

    fn handle_call(
        request: Self::Call,
        from: From,
        state: &mut Self::State,
    ) -> CallResult<Self::State, Self::Reply>;

    fn handle_cast(
        msg: Self::Cast,
        state: &mut Self::State,
    ) -> CastResult<Self::State>;

    fn handle_info(
        msg: Self::Info,  // or raw bytes
        state: &mut Self::State,
    ) -> InfoResult<Self::State>;

    fn terminate(reason: ExitReason, state: &mut Self::State) {}
}
```

### Return Types (matching Elixir)
```rust
pub enum InitResult<S> {
    Ok(S),
    OkTimeout(S, Duration),
    OkHibernate(S),
    OkContinue(S, ContinueArg),
    Ignore,
    Stop(ExitReason),
}

pub enum CallResult<S, R> {
    Reply(R, S),
    ReplyTimeout(R, S, Duration),
    ReplyContinue(R, S, ContinueArg),
    NoReply(S),
    NoReplyTimeout(S, Duration),
    NoReplyContinue(S, ContinueArg),
    Stop(ExitReason, R, S),
    StopNoReply(ExitReason, S),
}

pub enum CastResult<S> {
    NoReply(S),
    NoReplyTimeout(S, Duration),
    NoReplyContinue(S, ContinueArg),
    Stop(ExitReason, S),
}
```

### Client Functions
```rust
// Starting
async fn start<G: GenServer>(arg: G::InitArg) -> Result<Pid, StartError>;
async fn start_link<G: GenServer>(arg: G::InitArg) -> Result<Pid, StartError>;

// Messaging
async fn call<G: GenServer>(
    server: impl Into<ServerRef>,
    request: G::Call,
    timeout: Duration,
) -> Result<G::Reply, CallError>;

fn cast<G: GenServer>(server: impl Into<ServerRef>, msg: G::Cast);

fn reply(from: From, reply: impl Message);

async fn stop(server: impl Into<ServerRef>, reason: ExitReason) -> Result<(), StopError>;
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
    type InitArg: Send;

    fn init(arg: Self::InitArg) -> SupervisorInit;
}

pub struct SupervisorInit {
    pub strategy: Strategy,
    pub max_restarts: u32,      // Default: 3
    pub max_seconds: u32,       // Default: 5
    pub children: Vec<ChildSpec>,
}
```

### Client Functions
```rust
async fn start_link<S: Supervisor>(arg: S::InitArg) -> Result<Pid, StartError>;
async fn start_child(sup: Pid, spec: ChildSpec) -> Result<Pid, StartChildError>;
async fn terminate_child(sup: Pid, id: &str) -> Result<(), TerminateError>;
async fn restart_child(sup: Pid, id: &str) -> Result<Pid, RestartError>;
async fn delete_child(sup: Pid, id: &str) -> Result<(), DeleteError>;
async fn which_children(sup: Pid) -> Vec<ChildInfo>;
async fn count_children(sup: Pid) -> ChildCounts;
```

## Message Trait

```rust
pub trait Message: Serialize + DeserializeOwned + Send + 'static {
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError>;
}

// Blanket implementation for all Serialize + DeserializeOwned types
impl<T> Message for T
where
    T: Serialize + DeserializeOwned + Send + 'static
{ ... }
```

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

### GenServer Derive (future)
```rust
#[derive(GenServer)]
#[gen_server(call = MyCall, cast = MyCast, reply = MyReply)]
struct MyServer {
    // state fields
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

Each crate should have:
- Unit tests for core functionality
- Integration tests for cross-process behavior
- Doc tests for public APIs
- Examples demonstrating common patterns

## Build Commands

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests
cargo test -p ambitious-process    # Test specific crate
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
