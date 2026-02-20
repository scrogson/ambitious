# Core Correctness & API Ergonomics Design

**Date**: 2026-02-19
**Focus**: Core correctness + API ergonomics
**Approach**: Bottom-up (foundation first), cherry-picking best of spec and evolved code

## Overview

Fill in missing OTP-faithful APIs and improve developer ergonomics across all layers: Process, GenServer, Supervisor, and Application. Update documentation to match reality.

## Phase 1: Process API Surface

Expose existing `Context` methods as top-level `ambitious::` convenience functions. These already work internally; this phase is about wiring them into the public API.

### New functions in `runtime/task_local.rs`

```rust
pub fn link(pid: Pid) -> Result<(), SendError>
pub fn unlink(pid: Pid)
pub fn monitor(pid: Pid) -> Ref
pub fn demonitor(reference: Ref)
pub fn flag(flag: ProcessFlag, value: bool) -> bool
pub fn send_after(pid: Pid, msg: &impl Term, delay: Duration) -> TimerRef
```

### New type in `core/`

```rust
pub enum ProcessFlag {
    TrapExit,
}
```

### Re-exports

Add to `lib.rs` public exports and `prelude` module:
```rust
pub use runtime::{link, unlink, monitor, demonitor, flag, send_after};
pub use core::ProcessFlag;
```

### Implementation

Each function delegates to `with_ctx()` which accesses the task-local `ProcessScope`:

```rust
pub fn link(pid: Pid) -> Result<(), SendError> {
    with_ctx(|ctx| ctx.link(pid))
}
```

`send_after` delegates to `timer::send_after()` with the current process context.

## Phase 2: GenServer Ergonomics

### ServerRef

Unified way to address a GenServer by Pid or registered name.

```rust
pub enum ServerRef {
    Pid(Pid),
    Name(String),
}

impl ServerRef {
    pub fn resolve(&self) -> Option<Pid> {
        match self {
            ServerRef::Pid(pid) => Some(*pid),
            ServerRef::Name(name) => crate::whereis(name),
        }
    }
}

impl From<Pid> for ServerRef { ... }
impl From<&str> for ServerRef { ... }
impl From<String> for ServerRef { ... }
```

### Updated call/cast signatures

```rust
// Backwards-compatible: Pid still works via Into<ServerRef>
pub async fn call<G, R>(
    server: impl Into<ServerRef>,
    msg: G::Call,
    timeout: Duration,
) -> Result<R, Error>

pub fn cast<G>(server: impl Into<ServerRef>, msg: G::Cast)
```

### Public reply()

For deferred replies when `handle_call` returns `Reply::NoReply`:

```rust
pub fn reply<T: Term>(from: From, value: T)
```

Extracts the existing reply encoding logic from the server loop into a public function.

### Public stop()

Graceful GenServer shutdown:

```rust
pub async fn stop(
    server: impl Into<ServerRef>,
    reason: ExitReason,
) -> Result<(), Error>
```

Resolves the ServerRef and sends an exit signal.

## Phase 3: Supervisor Completeness

### Architecture: SupervisorCommand

Add command messages to the supervisor's message loop. Public functions send commands and await replies, keeping state mutation inside the supervisor process (OTP-faithful).

```rust
enum SupervisorCommand {
    TerminateChild { id: String, reply_to: Pid, reply_ref: Ref },
    DeleteChild { id: String, reply_to: Pid, reply_ref: Ref },
    RestartChild { id: String, reply_to: Pid, reply_ref: Ref },
}
```

The supervisor loop decodes these from its mailbox alongside DOWN/EXIT messages.

### terminate_child

Stops a running child, keeps its spec. Replace current stub.

1. Send `SupervisorCommand::TerminateChild` to supervisor
2. Supervisor looks up child, sends exit based on `ShutdownType`
3. Waits for termination (existing `wait_for_termination` helper)
4. Sets child pid to None, removes monitor
5. Updates `SUPERVISOR_STATES` query registry
6. Replies `Ok(())`

Idempotent: if child already stopped, returns `Ok(())`.

### delete_child

Removes a stopped child's spec. Replace current stub.

1. Send `SupervisorCommand::DeleteChild` to supervisor
2. Check child exists -> `DeleteError::NotFound`
3. Check child is stopped -> `DeleteError::Running`
4. Remove from `children` map and `child_order` vec
5. Update query registry
6. Reply `Ok(())`

### restart_child

Restart a terminated child using its existing spec. New function.

```rust
pub async fn restart_child(
    handle: &RuntimeHandle,
    sup: Pid,
    id: &str,
) -> Result<Pid, RestartError>
```

1. Send `SupervisorCommand::RestartChild` to supervisor
2. Check child exists -> `RestartError::NotFound`
3. Check child is stopped -> `RestartError::AlreadyRunning`
4. Call child's start function
5. Monitor new child, update state
6. Update query registry
7. Reply `Ok(new_pid)`

## Phase 4: Application Fixes

### Remove unsafe in AppController::start_with_config

Change `StartFn` and `StopFn` type aliases from `Box<dyn Fn...>` to `Arc<dyn Fn...>`:

```rust
type StartFn = Arc<dyn Fn(&RuntimeHandle, &AppConfig) -> Result<StartResult, String> + Send + Sync>;
type StopFn = Arc<dyn Fn(Option<Pid>) + Send + Sync>;
```

Then clone the Arc out of the lock scope and call safely. No unsafe needed.

### Fix stop_all reverse dependency ordering

Build the full dependency graph via `resolve_dependencies`, reverse it, and stop in that order. Dependents stop before their dependencies.

## Phase 5: Documentation Sync

### CLAUDE.md

- Update crate structure diagram (consolidated single crate)
- Update GenServer trait to struct-is-state design with `&mut self` handlers
- Update return type names: `Reply<T>`, `Status`, `Init<Self>`
- Add new Process functions (link, unlink, monitor, demonitor, flag, send_after)
- Add ServerRef, reply(), stop() to GenServer section
- Update Supervisor section with implemented terminate/delete/restart_child
- Update Message trait to actual encode_local/decode_local/encode_remote/decode_remote

### README.md

- Fix GenServer example to match current API
- Update crate structure diagram
- Add ServerRef usage example

## Testing Strategy

Each phase includes tests:

- **Phase 1**: Test each new top-level function (link, unlink, monitor, demonitor, flag, send_after) exercises the underlying Context method
- **Phase 2**: Test ServerRef resolution (Pid, Name, not-found), call/cast with name, reply() deferred, stop()
- **Phase 3**: Test terminate_child (running child, already stopped, not found), delete_child (stopped, running, not found), restart_child (stopped, running, not found)
- **Phase 4**: Verify AppController start/stop works without unsafe, verify stop_all order with dependency chain

## Non-Goals

- Distribution layer changes
- New features (GenEvent, Registry via-tuples, etc.)
- Performance optimization
- Benchmarks
- Breaking API changes to existing working functions
