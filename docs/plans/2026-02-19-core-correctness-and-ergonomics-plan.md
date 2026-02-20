# Core Correctness & API Ergonomics Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fill in missing OTP-faithful APIs and improve developer ergonomics across Process, GenServer, Supervisor, and Application layers.

**Architecture:** Bottom-up approach. Phase 1 exposes existing Context methods as top-level convenience functions. Phase 2 adds ServerRef and GenServer client ergonomics. Phase 3 implements supervisor child management via message-passing commands. Phase 4 fixes application safety and ordering. Phase 5 syncs documentation.

**Tech Stack:** Rust, tokio, dashmap, postcard, serde

---

## Task 1: Add ProcessFlag enum

**Files:**
- Create: `crates/ambitious/src/core/process_flag.rs`
- Modify: `crates/ambitious/src/core/mod.rs`

**Step 1: Create the ProcessFlag enum**

Create `crates/ambitious/src/core/process_flag.rs`:

```rust
//! Process flags for controlling process behavior.

/// Flags that control process behavior.
///
/// Used with `ambitious::flag()` to set process-level options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessFlag {
    /// When enabled, exit signals from linked processes are delivered
    /// as messages instead of terminating this process.
    TrapExit,
}
```

**Step 2: Export ProcessFlag from core module**

In `crates/ambitious/src/core/mod.rs`, add:
```rust
mod process_flag;
pub use process_flag::ProcessFlag;
```

Find the existing module declarations and add `mod process_flag;` alongside them, then add `ProcessFlag` to the `pub use` exports.

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors (ProcessFlag is defined but not yet used)

**Step 4: Commit**

```bash
git add crates/ambitious/src/core/process_flag.rs crates/ambitious/src/core/mod.rs
git commit -m "feat: add ProcessFlag enum to core types"
```

---

## Task 2: Add link, unlink, monitor, demonitor to task_local.rs

**Files:**
- Modify: `crates/ambitious/src/runtime/task_local.rs`

**Step 1: Write the failing test**

Add to the bottom of `crates/ambitious/src/runtime/task_local.rs` (inside a new `#[cfg(test)]` module if none exists, or at the file level for now since tests will be integration-level):

Actually, these functions delegate to `with_ctx` which requires a task-local context. We'll test them in Task 7 as integration tests. For now, write the implementations.

**Step 2: Add the four functions**

Add after the `exit()` function (around line 219) and before `set_exit_reason()`:

```rust
/// Creates a bidirectional link with another process.
///
/// If either process terminates abnormally, the other will receive
/// an exit signal.
///
/// # Panics
///
/// Panics if called outside of a Ambitious process context.
pub fn link(pid: Pid) -> Result<(), SendError> {
    with_ctx(|ctx| ctx.link(pid))
}

/// Removes a link with another process.
///
/// # Panics
///
/// Panics if called outside of a Ambitious process context.
pub fn unlink(pid: Pid) {
    with_ctx(|ctx| ctx.unlink(pid))
}

/// Creates a monitor on another process.
///
/// Returns a reference that will be included in the `DOWN` message
/// when the monitored process terminates.
///
/// # Panics
///
/// Panics if called outside of a Ambitious process context.
pub fn monitor(pid: Pid) -> Result<Ref, SendError> {
    with_ctx(|ctx| ctx.monitor(pid))
}

/// Removes a monitor.
///
/// The reference will no longer be valid and no `DOWN` message
/// will be sent for this monitor.
///
/// # Panics
///
/// Panics if called outside of a Ambitious process context.
pub fn demonitor(reference: Ref) {
    with_ctx(|ctx| ctx.demonitor(reference))
}
```

**Step 3: Add Ref import**

The `Ref` type is already available via `super::` imports. Check that `use super::{Context, Pid, SendError};` at line 7 covers what we need. `Ref` is not imported yet. Add it:

Change line 7 from:
```rust
use super::{Context, Pid, SendError};
```
to:
```rust
use super::{Context, Pid, Ref, SendError};
```

**Step 4: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 5: Commit**

```bash
git add crates/ambitious/src/runtime/task_local.rs
git commit -m "feat: add link, unlink, monitor, demonitor to task-local API"
```

---

## Task 3: Add flag function to task_local.rs

**Files:**
- Modify: `crates/ambitious/src/runtime/task_local.rs`

**Step 1: Add the flag function**

Add after the `demonitor()` function:

```rust
/// Sets a process flag and returns the previous value.
///
/// # Flags
///
/// - `ProcessFlag::TrapExit` - When `true`, exit signals from linked
///   processes are delivered as messages instead of terminating the process.
///
/// # Panics
///
/// Panics if called outside of a Ambitious process context.
pub fn flag(flag: ProcessFlag, value: bool) -> bool {
    match flag {
        ProcessFlag::TrapExit => with_ctx(|ctx| ctx.set_trap_exit(value)),
    }
}
```

**Step 2: Add ProcessFlag import**

Add to the imports at the top of the file:
```rust
use crate::core::ProcessFlag;
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 4: Commit**

```bash
git add crates/ambitious/src/runtime/task_local.rs
git commit -m "feat: add flag() function to task-local API"
```

---

## Task 4: Add send_after convenience function to task_local.rs

**Files:**
- Modify: `crates/ambitious/src/runtime/task_local.rs`

**Step 1: Add the send_after function**

Add after the `flag()` function:

```rust
/// Sends a message to a process after a delay.
///
/// Returns a [`TimerRef`] that can be used to cancel the timer.
///
/// This is a convenience wrapper around `timer::send_after` that takes
/// arguments in OTP order: `(pid, msg, delay)`.
///
/// # Panics
///
/// Panics if called outside of a Ambitious process context.
pub fn send_after<M: crate::core::Term>(
    pid: Pid,
    msg: &M,
    delay: std::time::Duration,
) -> crate::timer::TimerResult {
    crate::timer::send_after(delay, pid, msg)
}
```

**Step 2: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 3: Commit**

```bash
git add crates/ambitious/src/runtime/task_local.rs
git commit -m "feat: add send_after convenience function to task-local API"
```

---

## Task 5: Update runtime/mod.rs exports

**Files:**
- Modify: `crates/ambitious/src/runtime/mod.rs`

**Step 1: Add new function exports**

In `crates/ambitious/src/runtime/mod.rs`, update the `pub use task_local::` block (lines 28-31) from:

```rust
pub use task_local::{
    ProcessScope, current_pid, exit, recv, recv_timeout, send, send_raw, set_exit_reason,
    set_exit_reason_async, try_current_pid, try_recv, with_ctx, with_ctx_async,
};
```

to:

```rust
pub use task_local::{
    ProcessScope, current_pid, demonitor, exit, flag, link, monitor, recv, recv_timeout, send,
    send_after, send_raw, set_exit_reason, set_exit_reason_async, try_current_pid, try_recv,
    unlink, with_ctx, with_ctx_async,
};
```

**Step 2: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 3: Commit**

```bash
git add crates/ambitious/src/runtime/mod.rs
git commit -m "feat: export new process functions from runtime module"
```

---

## Task 6: Update lib.rs re-exports and prelude

**Files:**
- Modify: `crates/ambitious/src/lib.rs`

**Step 1: Update top-level re-exports**

In `crates/ambitious/src/lib.rs`, update the task-local re-exports block (lines 185-188) from:

```rust
pub use runtime::{
    current_pid, recv, recv_timeout, send, send_raw, try_current_pid, try_recv, with_ctx,
    with_ctx_async,
};
```

to:

```rust
pub use runtime::{
    current_pid, demonitor, flag, link, monitor, recv, recv_timeout, send, send_after, send_raw,
    try_current_pid, try_recv, unlink, with_ctx, with_ctx_async,
};
```

**Step 2: Add ProcessFlag to core re-exports**

Update the core types re-export line (line 191) from:

```rust
pub use core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, RawTerm, Ref, Term};
```

to:

```rust
pub use core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, ProcessFlag, RawTerm, Ref, Term};
```

**Step 3: Update prelude**

In the prelude module, update the task-local functions block (lines 231-234) from:

```rust
pub use crate::runtime::{
    current_pid, recv, recv_timeout, send, send_raw, try_current_pid, try_recv, with_ctx,
    with_ctx_async,
};
```

to:

```rust
pub use crate::runtime::{
    current_pid, demonitor, flag, link, monitor, recv, recv_timeout, send, send_after,
    send_raw, try_current_pid, try_recv, unlink, with_ctx, with_ctx_async,
};
```

Add `ProcessFlag` to the core types in prelude (line 208):

```rust
pub use crate::core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, ProcessFlag, RawTerm, Ref, Term};
```

**Step 4: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 5: Commit**

```bash
git add crates/ambitious/src/lib.rs
git commit -m "feat: re-export new process functions from lib.rs and prelude"
```

---

## Task 7: Write tests for Phase 1 process functions

**Files:**
- Modify: `crates/ambitious/src/lib.rs` (add integration test at bottom of existing tests)

**Step 1: Write the test**

Add the following test to the `#[cfg(test)] mod tests` block in `crates/ambitious/src/lib.rs` (after the existing tests):

```rust
#[tokio::test]
async fn test_process_api_convenience_functions() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let runtime = Runtime::new();
    let handle = runtime.handle();

    let link_ok = Arc::new(AtomicBool::new(false));
    let monitor_ok = Arc::new(AtomicBool::new(false));
    let flag_ok = Arc::new(AtomicBool::new(false));

    let link_ok_c = link_ok.clone();
    let monitor_ok_c = monitor_ok.clone();
    let flag_ok_c = flag_ok.clone();

    // Spawn a target process that stays alive
    let target_pid = handle.spawn(|| async {
        loop {
            if crate::recv_timeout(std::time::Duration::from_secs(10)).await.is_err() {
                break;
            }
        }
    });

    // Give target time to start
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    handle.spawn(move || async move {
        // Test link/unlink
        assert!(crate::link(target_pid).is_ok());
        crate::unlink(target_pid);
        link_ok_c.store(true, Ordering::SeqCst);

        // Test monitor/demonitor
        let mon_ref = crate::monitor(target_pid).unwrap();
        crate::demonitor(mon_ref);
        monitor_ok_c.store(true, Ordering::SeqCst);

        // Test flag
        let prev = crate::flag(ProcessFlag::TrapExit, true);
        assert!(!prev); // Was false before
        let prev2 = crate::flag(ProcessFlag::TrapExit, false);
        assert!(prev2); // Was true
        flag_ok_c.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(link_ok.load(Ordering::SeqCst), "link/unlink should work");
    assert!(monitor_ok.load(Ordering::SeqCst), "monitor/demonitor should work");
    assert!(flag_ok.load(Ordering::SeqCst), "flag should work");
}
```

**Step 2: Run the test**

Run: `cargo test -p ambitious test_process_api_convenience_functions -- --nocapture`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/ambitious/src/lib.rs
git commit -m "test: add tests for new process API convenience functions"
```

---

## Task 8: Run full CI for Phase 1

**Step 1: Run CI**

Run: `just ci`
Expected: All tests pass, no warnings

**Step 2: Fix any issues and commit**

If any issues, fix and commit with appropriate message.

---

## Task 9: Add ServerRef type

**Files:**
- Create: `crates/ambitious/src/gen_server/server_ref.rs`
- Modify: `crates/ambitious/src/gen_server/mod.rs`

**Step 1: Create ServerRef module**

Create `crates/ambitious/src/gen_server/server_ref.rs`:

```rust
//! ServerRef - unified way to address a GenServer.

use crate::core::Pid;

/// A reference to a GenServer, either by PID or registered name.
///
/// This allows GenServer client functions (`call`, `cast`, `stop`) to
/// accept either a `Pid` or a name string.
///
/// # Examples
///
/// ```ignore
/// use ambitious::gen_server::{call, ServerRef};
///
/// // By PID (most common)
/// let reply = call::<MyServer, _>(pid, msg, timeout).await?;
///
/// // By registered name
/// let reply = call::<MyServer, _>("my_server", msg, timeout).await?;
/// ```
#[derive(Debug, Clone)]
pub enum ServerRef {
    /// Reference by process ID.
    Pid(Pid),
    /// Reference by registered name.
    Name(String),
}

impl ServerRef {
    /// Resolves the ServerRef to a Pid.
    ///
    /// Returns `None` if the name is not registered.
    pub fn resolve(&self) -> Option<Pid> {
        match self {
            ServerRef::Pid(pid) => Some(*pid),
            ServerRef::Name(name) => crate::whereis(name),
        }
    }
}

impl From<Pid> for ServerRef {
    fn from(pid: Pid) -> Self {
        ServerRef::Pid(pid)
    }
}

impl From<&str> for ServerRef {
    fn from(name: &str) -> Self {
        ServerRef::Name(name.to_string())
    }
}

impl From<String> for ServerRef {
    fn from(name: String) -> Self {
        ServerRef::Name(name)
    }
}

impl std::fmt::Display for ServerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerRef::Pid(pid) => write!(f, "{:?}", pid),
            ServerRef::Name(name) => write!(f, "{}", name),
        }
    }
}
```

**Step 2: Export from gen_server mod**

In `crates/ambitious/src/gen_server/mod.rs`, add after `mod types;` (line 88):

```rust
mod server_ref;
```

And update the exports at line 91 from:

```rust
pub use server::{Error, call, cast, start, start_link};
```

to:

```rust
pub use server::{Error, call, cast, start, start_link};
pub use server_ref::ServerRef;
```

Also update the gen_server prelude (line 108-111) to include `ServerRef`:

```rust
pub use super::{
    Error, ExitReason, From, GenServer, Init, Pid, Reply, ServerRef, Status, async_trait,
    call, cast, start, start_link,
};
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 4: Commit**

```bash
git add crates/ambitious/src/gen_server/server_ref.rs crates/ambitious/src/gen_server/mod.rs
git commit -m "feat: add ServerRef type for addressing GenServers by PID or name"
```

---

## Task 10: Update call and cast to accept ServerRef

**Files:**
- Modify: `crates/ambitious/src/gen_server/server.rs`

**Step 1: Update call signature**

In `crates/ambitious/src/gen_server/server.rs`, change the `call` function (line 354) from:

```rust
pub async fn call<G, R>(pid: Pid, msg: G::Call, timeout: Duration) -> Result<R, Error>
where
    G: GenServer,
    R: Message,
{
    use tokio::time::timeout as tokio_timeout;

    let reference = Ref::new();
    let caller_pid = crate::current_pid();
```

to:

```rust
pub async fn call<G, R>(
    server: impl Into<super::ServerRef>,
    msg: G::Call,
    timeout: Duration,
) -> Result<R, Error>
where
    G: GenServer,
    R: Message,
{
    use tokio::time::timeout as tokio_timeout;

    let pid = server.into().resolve().ok_or(Error::NotFound)?;
    let reference = Ref::new();
    let caller_pid = crate::current_pid();
```

**Step 2: Update cast signature**

Change the `cast` function (line 421) from:

```rust
pub fn cast<G>(pid: Pid, msg: G::Cast)
where
    G: GenServer,
{
    // Wrap in $gen_cast envelope
    let envelope = encode_gen_cast_envelope(msg.encode_local());
    let _ = crate::send_raw(pid, envelope);
}
```

to:

```rust
pub fn cast<G>(server: impl Into<super::ServerRef>, msg: G::Cast)
where
    G: GenServer,
{
    let Some(pid) = server.into().resolve() else {
        return;
    };
    // Wrap in $gen_cast envelope
    let envelope = encode_gen_cast_envelope(msg.encode_local());
    let _ = crate::send_raw(pid, envelope);
}
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors (existing callers pass `Pid` which implements `Into<ServerRef>`)

**Step 4: Run existing tests**

Run: `cargo test -p ambitious gen_server`
Expected: All existing tests pass (backward compatible via `From<Pid>`)

**Step 5: Commit**

```bash
git add crates/ambitious/src/gen_server/server.rs
git commit -m "feat: update call/cast to accept impl Into<ServerRef>"
```

---

## Task 11: Add public reply() function

**Files:**
- Modify: `crates/ambitious/src/gen_server/server.rs`
- Modify: `crates/ambitious/src/gen_server/mod.rs`

**Step 1: Add the public reply function**

In `crates/ambitious/src/gen_server/server.rs`, add after the `cast` function:

```rust
/// Sends a reply to a caller outside of the normal handle_call return flow.
///
/// This is useful when `handle_call` returns `Reply::NoReply` and the
/// reply needs to be sent later (deferred reply pattern).
///
/// # Example
///
/// ```ignore
/// async fn handle_call(&mut self, msg: MyCall, from: From) -> Reply<MyReply> {
///     // Store `from` and reply later
///     self.pending = Some(from);
///     Reply::NoReply
/// }
///
/// // Later, in handle_cast or handle_info:
/// gen_server::reply(&self.pending.take().unwrap(), MyReply::Done);
/// ```
pub fn reply<T: Message>(from: &From, value: T) {
    send_reply(from, value.encode_local());
}
```

Note: `send_reply` already exists as a private function at line 304. The new `reply` function just exposes it publicly with a typed interface.

**Step 2: Export reply from gen_server mod**

In `crates/ambitious/src/gen_server/mod.rs`, update line 91 from:

```rust
pub use server::{Error, call, cast, start, start_link};
```

to:

```rust
pub use server::{Error, call, cast, reply, start, start_link};
```

Update the prelude too (the `pub use super::` block):

```rust
pub use super::{
    Error, ExitReason, From, GenServer, Init, Pid, Reply, ServerRef, Status, async_trait,
    call, cast, reply, start, start_link,
};
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 4: Commit**

```bash
git add crates/ambitious/src/gen_server/server.rs crates/ambitious/src/gen_server/mod.rs
git commit -m "feat: add public reply() function for deferred GenServer replies"
```

---

## Task 12: Add public stop() function

**Files:**
- Modify: `crates/ambitious/src/gen_server/server.rs`
- Modify: `crates/ambitious/src/gen_server/mod.rs`

**Step 1: Add the stop function**

In `crates/ambitious/src/gen_server/server.rs`, add after the `reply` function:

```rust
/// Gracefully stops a GenServer.
///
/// Sends a stop message to the server and waits for acknowledgment.
/// The server's `terminate` callback will be called with the given reason.
///
/// # Example
///
/// ```ignore
/// gen_server::stop(pid, ExitReason::Normal).await?;
/// gen_server::stop("my_server", ExitReason::Shutdown).await?;
/// ```
pub async fn stop(
    server: impl Into<super::ServerRef>,
    reason: ExitReason,
) -> Result<(), Error> {
    let pid = server.into().resolve().ok_or(Error::NotFound)?;

    let reference = Ref::new();
    let caller_pid = crate::current_pid();

    let from = From {
        pid: caller_pid,
        reference,
    };

    // Send a Stop protocol message with a from handle for reply
    let stop_msg = super::protocol::encode_stop(reason, Some(from));
    let _ = crate::send_raw(pid, stop_msg);

    // Wait for acknowledgment with a reasonable timeout
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_reply::<()>(reference),
    )
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Timeout),
    }
}
```

Note: The `()` type needs to implement `Message`. Check if it already does. Looking at the codebase, `()` should implement `Message` via the derive or blanket impl. If not, we may need to handle this differently. The existing server loop already handles `ProtocolMessage::Stop` at line 200-206 of `server.rs` and sends a reply using `send_reply(&f, Term::encode(&()))`. So the reply payload is `Term::encode(&())` which is the raw encoding. The `wait_for_reply` function decodes using `Message::decode_local`. We need `()` to implement `Message`.

Let's check - if `()` doesn't implement `Message`, we can use a simpler approach: just send the exit signal and don't wait for reply. Actually, looking at the stop handler in `gen_server_loop`:

```rust
ProtocolMessage::Stop { reason, from } => {
    if let Some(f) = from {
        send_reply(&f, Term::encode(&()));
    }
    server.terminate(reason).await;
    return;
}
```

The reply is `Term::encode(&())` which is raw bytes, then `wait_for_reply::<()>` would try to decode it via `Message::decode_local`. The `()` type likely doesn't implement `Message`. Let's change the approach to just send an exit signal:

```rust
/// Gracefully stops a GenServer.
///
/// Sends an exit signal with the given reason to the server process.
/// The server's `terminate` callback will be called.
///
/// # Example
///
/// ```ignore
/// gen_server::stop(pid, ExitReason::Normal).await?;
/// gen_server::stop("my_server", ExitReason::Shutdown).await?;
/// ```
pub async fn stop(
    server: impl Into<super::ServerRef>,
    reason: ExitReason,
) -> Result<(), Error> {
    let pid = server.into().resolve().ok_or(Error::NotFound)?;

    // Send a Stop protocol message
    let stop_msg = super::protocol::encode_stop(reason, None);
    crate::send_raw(pid, stop_msg).map_err(|_| Error::NotFound)?;

    Ok(())
}
```

**Step 2: Export stop from gen_server mod**

In `crates/ambitious/src/gen_server/mod.rs`, update the exports:

```rust
pub use server::{Error, call, cast, reply, start, start_link, stop};
```

Update the prelude:

```rust
pub use super::{
    Error, ExitReason, From, GenServer, Init, Pid, Reply, ServerRef, Status, async_trait,
    call, cast, reply, start, start_link, stop,
};
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 4: Commit**

```bash
git add crates/ambitious/src/gen_server/server.rs crates/ambitious/src/gen_server/mod.rs
git commit -m "feat: add public stop() function for graceful GenServer shutdown"
```

---

## Task 13: Update lib.rs for GenServer re-exports

**Files:**
- Modify: `crates/ambitious/src/lib.rs`

**Step 1: Add ServerRef to prelude**

In `crates/ambitious/src/lib.rs`, update the prelude's GenServer section (lines 215-217) from:

```rust
pub use crate::gen_server::{
    Error, From, GenServer, Init, Reply, Status, call, cast, start, start_link,
};
```

to:

```rust
pub use crate::gen_server::{
    Error, From, GenServer, Init, Reply, ServerRef, Status, call, cast, reply, start,
    start_link, stop,
};
```

**Step 2: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 3: Commit**

```bash
git add crates/ambitious/src/lib.rs
git commit -m "feat: add ServerRef, reply, stop to lib.rs prelude"
```

---

## Task 14: Write tests for Phase 2 GenServer ergonomics

**Files:**
- Modify: `crates/ambitious/src/gen_server/mod.rs`

**Step 1: Add ServerRef and stop tests**

Add to the existing `#[cfg(test)] mod tests` in `crates/ambitious/src/gen_server/mod.rs`:

```rust
#[test]
fn test_server_ref_from_pid() {
    let pid = crate::core::Pid::new();
    let sr: ServerRef = pid.into();
    match sr {
        ServerRef::Pid(p) => assert_eq!(p, pid),
        _ => panic!("expected Pid variant"),
    }
}

#[test]
fn test_server_ref_from_str() {
    let sr: ServerRef = "my_server".into();
    match sr {
        ServerRef::Name(n) => assert_eq!(n, "my_server"),
        _ => panic!("expected Name variant"),
    }
}

#[test]
fn test_server_ref_from_string() {
    let sr: ServerRef = String::from("my_server").into();
    match sr {
        ServerRef::Name(n) => assert_eq!(n, "my_server"),
        _ => panic!("expected Name variant"),
    }
}

#[test]
fn test_server_ref_resolve_unregistered() {
    crate::init();
    let sr = ServerRef::Name("nonexistent_server".to_string());
    assert!(sr.resolve().is_none());
}

#[tokio::test]
async fn test_call_with_server_ref_pid() {
    crate::init();
    let handle = crate::handle();

    let result = Arc::new(AtomicI64::new(-1));
    let result_clone = result.clone();

    handle.spawn(move || async move {
        let pid = start::<Counter>(42).await.expect("failed to start counter");

        // Call using Pid directly (backwards compatible)
        let reply: CounterReply =
            call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5))
                .await
                .expect("call failed");

        result_clone.store(reply.0, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(result.load(Ordering::SeqCst), 42);
}

#[tokio::test]
async fn test_stop_gen_server() {
    use crate::gen_server::stop;

    crate::init();
    let handle = crate::handle();

    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped_clone = stopped.clone();

    handle.spawn(move || async move {
        let pid = start::<Counter>(0).await.expect("failed to start counter");
        assert!(crate::alive(pid));

        stop(pid, crate::core::ExitReason::Normal).await.unwrap();

        // Give it time to process
        tokio::time::sleep(Duration::from_millis(50)).await;

        stopped_clone.store(!crate::alive(pid), Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(stopped.load(Ordering::SeqCst), "Server should have stopped");
}
```

**Step 2: Run the tests**

Run: `cargo test -p ambitious gen_server`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/ambitious/src/gen_server/mod.rs
git commit -m "test: add tests for ServerRef, call with ServerRef, and stop"
```

---

## Task 15: Run full CI for Phase 2

**Step 1: Run CI**

Run: `just ci`
Expected: All tests pass, no warnings

**Step 2: Fix any issues and commit**

---

## Task 16: Add SupervisorCommand enum and handler

**Files:**
- Modify: `crates/ambitious/src/supervisor/core.rs`

**Step 1: Define the SupervisorCommand enum**

Add after the `SupervisorQueryState` impl block (around line 76), before the `Supervisor` trait:

```rust
/// Commands sent to a supervisor process for child management.
///
/// Public functions send these commands to the supervisor's mailbox and
/// await replies, keeping state mutation inside the supervisor process.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum SupervisorCommand {
    /// Terminate a running child, keeping its spec.
    TerminateChild {
        /// The child ID to terminate.
        id: String,
        /// PID to send the reply to.
        reply_to: Pid,
        /// Reference for the reply.
        reply_ref: Ref,
    },
    /// Delete a stopped child's spec.
    DeleteChild {
        /// The child ID to delete.
        id: String,
        /// PID to send the reply to.
        reply_to: Pid,
        /// Reference for the reply.
        reply_ref: Ref,
    },
    /// Restart a stopped child using its existing spec.
    RestartChild {
        /// The child ID to restart.
        id: String,
        /// PID to send the reply to.
        reply_to: Pid,
        /// Reference for the reply.
        reply_ref: Ref,
    },
}

/// Tag for supervisor command messages.
const SUPERVISOR_COMMAND_TAG: &str = "$supervisor_cmd";

/// Encode a supervisor command as a tagged message.
fn encode_supervisor_command(cmd: &SupervisorCommand) -> Vec<u8> {
    use crate::core::Term;
    let payload = Term::encode(cmd);
    let tagged = crate::gen_server::protocol::TaggedEnvelope::new(SUPERVISOR_COMMAND_TAG, payload);
    tagged.encode()
}

/// Try to decode a supervisor command from raw bytes.
fn decode_supervisor_command(data: &[u8]) -> Option<SupervisorCommand> {
    use crate::core::Term;
    let tagged = crate::gen_server::protocol::TaggedEnvelope::decode(data).ok()?;
    if tagged.tag != SUPERVISOR_COMMAND_TAG {
        return None;
    }
    Term::decode(&tagged.payload).ok()
}
```

**Step 2: Add `use crate::core::Ref` if not already imported**

Check the imports at the top of `core.rs`. Line 10 already has:
```rust
use crate::core::{ExitReason, Pid, Ref, SystemMessage, Term};
```

`Ref` is already imported.

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles (may have dead code warnings for the new functions, that's ok)

**Step 4: Commit**

```bash
git add crates/ambitious/src/supervisor/core.rs
git commit -m "feat: add SupervisorCommand enum for supervisor child management"
```

---

## Task 17: Add command handling to supervisor_loop

**Files:**
- Modify: `crates/ambitious/src/supervisor/core.rs`

**Step 1: Add command handling in the supervisor loop**

In the `supervisor_loop` function (line 388), add command handling after the EXIT signal check (after line 439) and before the closing `}` of the loop:

```rust
        // Check for supervisor commands
        if let Some(cmd) = decode_supervisor_command(&msg) {
            match cmd {
                SupervisorCommand::TerminateChild { id, reply_to, reply_ref } => {
                    let result = state.handle_terminate_child_command(&id).await;
                    state.sync_to_registry();
                    let reply_bytes = Term::encode(&result);
                    let reply_msg = crate::gen_server::protocol::encode_reply(reply_ref, &reply_bytes);
                    let _ = crate::send_raw(reply_to, reply_msg);
                }
                SupervisorCommand::DeleteChild { id, reply_to, reply_ref } => {
                    let result = state.handle_delete_child_command(&id);
                    state.sync_to_registry();
                    let reply_bytes = Term::encode(&result);
                    let reply_msg = crate::gen_server::protocol::encode_reply(reply_ref, &reply_bytes);
                    let _ = crate::send_raw(reply_to, reply_msg);
                }
                SupervisorCommand::RestartChild { id, reply_to, reply_ref } => {
                    let result = state.handle_restart_child_command(&id).await;
                    state.sync_to_registry();
                    let reply_bytes = Term::encode(&result);
                    let reply_msg = crate::gen_server::protocol::encode_reply(reply_ref, &reply_bytes);
                    let _ = crate::send_raw(reply_to, reply_msg);
                }
            }
        }
```

**Step 2: Add the handler methods to SupervisorState**

Add these methods to the `impl SupervisorState` block:

```rust
    /// Handles a terminate_child command.
    /// Returns Ok(()) if child was terminated or already stopped.
    /// Returns Err with message if child not found.
    async fn handle_terminate_child_command(&mut self, id: &str) -> Result<(), String> {
        if !self.children.contains_key(id) {
            return Err(format!("child '{}' not found", id));
        }

        // Terminate the child (idempotent - ok if already stopped)
        self.terminate_child_by_id(id).await;
        Ok(())
    }

    /// Handles a delete_child command.
    /// Returns Ok(()) if child spec was deleted.
    /// Returns Err with message if child not found or still running.
    fn handle_delete_child_command(&mut self, id: &str) -> Result<(), String> {
        let child = self.children.get(id)
            .ok_or_else(|| format!("not_found:{}", id))?;

        if child.pid.is_some() {
            return Err(format!("running:{}", id));
        }

        // Remove from children map and child_order
        self.children.remove(id);
        self.child_order.retain(|i| i != id);

        Ok(())
    }

    /// Handles a restart_child command.
    /// Returns Ok(pid) if child was restarted.
    /// Returns Err with message if child not found, already running, or failed to start.
    async fn handle_restart_child_command(&mut self, id: &str) -> Result<Pid, String> {
        let child = self.children.get(id)
            .ok_or_else(|| format!("not_found:{}", id))?;

        if child.pid.is_some() {
            return Err(format!("already_running:{}", id));
        }

        // Start the child using its existing spec
        self.start_child(id).await
    }
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 4: Commit**

```bash
git add crates/ambitious/src/supervisor/core.rs
git commit -m "feat: add command handling to supervisor loop for child management"
```

---

## Task 18: Implement terminate_child via command

**Files:**
- Modify: `crates/ambitious/src/supervisor/core.rs`

**Step 1: Replace the terminate_child stub**

Replace the existing `terminate_child` function (lines 582-589) with:

```rust
/// Terminates a child process managed by the supervisor.
///
/// The child's spec is kept, so it can be restarted later with `restart_child`.
/// If the child is already stopped, returns `Ok(())` (idempotent).
///
/// This function sends a command to the supervisor process and waits for a reply.
pub async fn terminate_child(
    _handle: &RuntimeHandle,
    sup: Pid,
    id: &str,
) -> Result<(), TerminateError> {
    let reply_ref = Ref::new();
    let caller_pid = crate::runtime::current_pid();

    let cmd = SupervisorCommand::TerminateChild {
        id: id.to_string(),
        reply_to: caller_pid,
        reply_ref,
    };

    // Send command to supervisor
    crate::send_raw(sup, encode_supervisor_command(&cmd))
        .map_err(|_| TerminateError::NotFound(id.to_string()))?;

    // Wait for reply
    let reply = wait_for_supervisor_reply(reply_ref).await
        .map_err(|_| TerminateError::Timeout)?;

    // Decode result
    let result: Result<(), String> = Term::decode(&reply)
        .map_err(|_| TerminateError::NotFound(id.to_string()))?;

    result.map_err(|e| TerminateError::NotFound(e))
}
```

**Step 2: Add the wait_for_supervisor_reply helper**

Add this helper function near the bottom of the file, before the `wait_for_termination` function:

```rust
/// Waits for a reply from the supervisor matching the given reference.
async fn wait_for_supervisor_reply(reference: Ref) -> Result<Vec<u8>, ()> {
    use crate::gen_server::protocol::Message as ProtocolMessage;

    let timeout = std::time::Duration::from_secs(5);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline - tokio::time::Instant::now();
        if remaining.is_zero() {
            return Err(());
        }

        let raw_msg = match crate::runtime::recv_timeout(remaining).await {
            Ok(Some(m)) => m,
            Ok(None) => return Err(()), // Mailbox closed
            Err(()) => return Err(()),  // Timeout
        };

        // Check if this is our reply
        if let Ok(ProtocolMessage::Reply {
            reference: ref r,
            payload,
        }) = ProtocolMessage::decode(&raw_msg)
        {
            if *r == reference {
                return Ok(payload);
            }
        }

        // Not our reply - put it back? No, just log and continue.
        // In a production system we'd want selective receive, but for now
        // supervisor commands are typically sent from dedicated callers.
        tracing::trace!("supervisor: ignoring non-reply message while waiting for command reply");
    }
}
```

**Step 3: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 4: Commit**

```bash
git add crates/ambitious/src/supervisor/core.rs
git commit -m "feat: implement terminate_child via supervisor command"
```

---

## Task 19: Implement delete_child via command

**Files:**
- Modify: `crates/ambitious/src/supervisor/core.rs`

**Step 1: Replace the delete_child stub**

Replace the existing `delete_child` function (lines 594-601, now shifted) with:

```rust
/// Deletes a stopped child's specification from the supervisor.
///
/// The child must be stopped first (via `terminate_child`).
/// Returns an error if the child is still running or not found.
pub async fn delete_child(
    _handle: &RuntimeHandle,
    sup: Pid,
    id: &str,
) -> Result<(), DeleteError> {
    let reply_ref = Ref::new();
    let caller_pid = crate::runtime::current_pid();

    let cmd = SupervisorCommand::DeleteChild {
        id: id.to_string(),
        reply_to: caller_pid,
        reply_ref,
    };

    // Send command to supervisor
    crate::send_raw(sup, encode_supervisor_command(&cmd))
        .map_err(|_| DeleteError::NotFound(id.to_string()))?;

    // Wait for reply
    let reply = wait_for_supervisor_reply(reply_ref).await
        .map_err(|_| DeleteError::NotFound(id.to_string()))?;

    // Decode result
    let result: Result<(), String> = Term::decode(&reply)
        .map_err(|_| DeleteError::NotFound(id.to_string()))?;

    result.map_err(|e| {
        if e.starts_with("running:") {
            DeleteError::Running(e.strip_prefix("running:").unwrap_or(&e).to_string())
        } else {
            DeleteError::NotFound(e.strip_prefix("not_found:").unwrap_or(&e).to_string())
        }
    })
}
```

**Step 2: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 3: Commit**

```bash
git add crates/ambitious/src/supervisor/core.rs
git commit -m "feat: implement delete_child via supervisor command"
```

---

## Task 20: Add restart_child function

**Files:**
- Modify: `crates/ambitious/src/supervisor/core.rs`
- Modify: `crates/ambitious/src/supervisor/mod.rs`

**Step 1: Add the restart_child public function**

Add after the `delete_child` function:

```rust
/// Restarts a terminated child using its existing specification.
///
/// The child must be stopped first. Returns the new PID of the restarted child.
pub async fn restart_child(
    _handle: &RuntimeHandle,
    sup: Pid,
    id: &str,
) -> Result<Pid, RestartError> {
    let reply_ref = Ref::new();
    let caller_pid = crate::runtime::current_pid();

    let cmd = SupervisorCommand::RestartChild {
        id: id.to_string(),
        reply_to: caller_pid,
        reply_ref,
    };

    // Send command to supervisor
    crate::send_raw(sup, encode_supervisor_command(&cmd))
        .map_err(|_| RestartError::NotFound(id.to_string()))?;

    // Wait for reply
    let reply = wait_for_supervisor_reply(reply_ref).await
        .map_err(|_| RestartError::StartFailed("timeout waiting for reply".to_string()))?;

    // Decode result
    let result: Result<Pid, String> = Term::decode(&reply)
        .map_err(|_| RestartError::StartFailed("decode error".to_string()))?;

    result.map_err(|e| {
        if e.starts_with("already_running:") {
            RestartError::AlreadyRunning(
                e.strip_prefix("already_running:").unwrap_or(&e).to_string(),
            )
        } else if e.starts_with("not_found:") {
            RestartError::NotFound(e.strip_prefix("not_found:").unwrap_or(&e).to_string())
        } else {
            RestartError::StartFailed(e)
        }
    })
}
```

**Step 2: Add RestartError import**

Check that `RestartError` is imported at the top of `core.rs`. Line 5 has:
```rust
use super::error::{DeleteError, StartError, TerminateError};
```

Add `RestartError`:
```rust
use super::error::{DeleteError, RestartError, StartError, TerminateError};
```

**Step 3: Export restart_child from supervisor/mod.rs**

In `crates/ambitious/src/supervisor/mod.rs`, update line 66-69 from:

```rust
pub use core::{
    Supervisor, SupervisorInit, count_children, delete_child, start, start_link, terminate_child,
    which_children,
};
```

to:

```rust
pub use core::{
    Supervisor, SupervisorInit, count_children, delete_child, restart_child, start, start_link,
    terminate_child, which_children,
};
```

**Step 4: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors

**Step 5: Commit**

```bash
git add crates/ambitious/src/supervisor/core.rs crates/ambitious/src/supervisor/mod.rs
git commit -m "feat: add restart_child function for restarting terminated children"
```

---

## Task 21: Write tests for Phase 3 supervisor commands

**Files:**
- Modify: `crates/ambitious/src/supervisor/mod.rs`

**Step 1: Add terminate/delete/restart_child tests**

Add to the existing test module in `crates/ambitious/src/supervisor/mod.rs`:

```rust
#[tokio::test]
async fn test_terminate_child() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let runtime = Runtime::new();
    let handle = runtime.handle();

    let child_started = Arc::new(AtomicBool::new(false));
    let child_started_clone = child_started.clone();

    struct TermTestSupervisor;

    impl Supervisor for TermTestSupervisor {
        type InitArg = (RuntimeHandle, Arc<AtomicBool>);

        fn init((handle, started_flag): Self::InitArg) -> SupervisorInit {
            let handle_clone = handle.clone();
            SupervisorInit::new(
                SupervisorFlags::new(Strategy::OneForOne)
                    .max_restarts(0)
                    .max_seconds(5),
                vec![ChildSpec::new("worker1", move || {
                    let h = handle_clone.clone();
                    let sf = started_flag.clone();
                    async move {
                        let pid = h.spawn(move || async move {
                            sf.store(true, Ordering::SeqCst);
                            while let Ok(Some(_)) =
                                crate::recv_timeout(Duration::from_secs(60)).await
                            {
                            }
                        });
                        Ok(pid)
                    }
                })
                .restart(RestartType::Temporary)],
            )
        }
    }

    // Need to spawn within a process context for terminate_child
    let test_passed = Arc::new(AtomicBool::new(false));
    let test_passed_clone = test_passed.clone();
    let handle_clone = handle.clone();

    handle.spawn(move || async move {
        let sup_pid =
            start::<TermTestSupervisor>(&handle_clone, (handle_clone.clone(), child_started_clone))
                .await
                .unwrap();

        sleep(Duration::from_millis(50)).await;

        // Terminate the child
        let result = terminate_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result.is_ok(), "terminate_child should succeed: {:?}", result);

        // Terminating again should be idempotent
        let result2 = terminate_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result2.is_ok(), "terminate_child should be idempotent");

        // Not found
        let result3 = terminate_child(&handle_clone, sup_pid, "nonexistent").await;
        assert!(result3.is_err(), "terminate_child should error for nonexistent");

        test_passed_clone.store(true, Ordering::SeqCst);
    });

    sleep(Duration::from_millis(500)).await;
    assert!(
        test_passed.load(Ordering::SeqCst),
        "terminate_child test should pass"
    );
}

#[tokio::test]
async fn test_delete_child() {
    let runtime = Runtime::new();
    let handle = runtime.handle();

    struct DeleteTestSupervisor;

    impl Supervisor for DeleteTestSupervisor {
        type InitArg = RuntimeHandle;

        fn init(handle: Self::InitArg) -> SupervisorInit {
            let handle_clone = handle.clone();
            SupervisorInit::new(
                SupervisorFlags::new(Strategy::OneForOne)
                    .max_restarts(0)
                    .max_seconds(5),
                vec![ChildSpec::new("worker1", move || {
                    let h = handle_clone.clone();
                    async move {
                        let pid = h.spawn(|| async {
                            while let Ok(Some(_)) =
                                crate::recv_timeout(Duration::from_secs(60)).await
                            {
                            }
                        });
                        Ok(pid)
                    }
                })
                .restart(RestartType::Temporary)],
            )
        }
    }

    let test_passed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let test_passed_clone = test_passed.clone();
    let handle_clone = handle.clone();

    handle.spawn(move || async move {
        let sup_pid = start::<DeleteTestSupervisor>(&handle_clone, handle_clone.clone())
            .await
            .unwrap();

        sleep(Duration::from_millis(50)).await;

        // Can't delete a running child
        let result = delete_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result.is_err(), "delete_child should fail for running child");

        // Terminate first, then delete
        terminate_child(&handle_clone, sup_pid, "worker1")
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;

        let result2 = delete_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result2.is_ok(), "delete_child should succeed after terminate: {:?}", result2);

        // Delete again should fail (not found)
        let result3 = delete_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result3.is_err(), "delete_child should fail after already deleted");

        test_passed_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    sleep(Duration::from_millis(500)).await;
    assert!(
        test_passed.load(std::sync::atomic::Ordering::SeqCst),
        "delete_child test should pass"
    );
}

#[tokio::test]
async fn test_restart_child() {
    let runtime = Runtime::new();
    let handle = runtime.handle();

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let start_count = Arc::new(AtomicUsize::new(0));
    let start_count_clone = start_count.clone();

    struct RestartTestSupervisor;

    impl Supervisor for RestartTestSupervisor {
        type InitArg = (RuntimeHandle, Arc<AtomicUsize>);

        fn init((handle, count): Self::InitArg) -> SupervisorInit {
            let handle_clone = handle.clone();
            SupervisorInit::new(
                SupervisorFlags::new(Strategy::OneForOne)
                    .max_restarts(0)
                    .max_seconds(5),
                vec![ChildSpec::new("worker1", move || {
                    let h = handle_clone.clone();
                    let c = count.clone();
                    async move {
                        let pid = h.spawn(move || async move {
                            c.fetch_add(1, Ordering::SeqCst);
                            while let Ok(Some(_)) =
                                crate::recv_timeout(Duration::from_secs(60)).await
                            {
                            }
                        });
                        Ok(pid)
                    }
                })
                .restart(RestartType::Temporary)],
            )
        }
    }

    let test_passed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let test_passed_clone = test_passed.clone();
    let handle_clone = handle.clone();

    handle.spawn(move || async move {
        let sup_pid = start::<RestartTestSupervisor>(
            &handle_clone,
            (handle_clone.clone(), start_count_clone),
        )
        .await
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        assert_eq!(start_count.load(Ordering::SeqCst), 1, "Should have started once");

        // Can't restart a running child
        let result =
            crate::supervisor::restart_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result.is_err(), "restart_child should fail for running child");

        // Terminate, then restart
        terminate_child(&handle_clone, sup_pid, "worker1")
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;

        let result2 =
            crate::supervisor::restart_child(&handle_clone, sup_pid, "worker1").await;
        assert!(result2.is_ok(), "restart_child should succeed: {:?}", result2);

        sleep(Duration::from_millis(50)).await;
        assert_eq!(
            start_count.load(Ordering::SeqCst),
            2,
            "Should have started twice"
        );

        test_passed_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    sleep(Duration::from_millis(1000)).await;
    assert!(
        test_passed.load(std::sync::atomic::Ordering::SeqCst),
        "restart_child test should pass"
    );
}
```

**Step 2: Run the tests**

Run: `cargo test -p ambitious supervisor::tests::test_terminate_child -- --nocapture`
Run: `cargo test -p ambitious supervisor::tests::test_delete_child -- --nocapture`
Run: `cargo test -p ambitious supervisor::tests::test_restart_child -- --nocapture`
Expected: All pass

**Step 3: Commit**

```bash
git add crates/ambitious/src/supervisor/mod.rs
git commit -m "test: add tests for terminate_child, delete_child, restart_child"
```

---

## Task 22: Run full CI for Phase 3

**Step 1: Run CI**

Run: `just ci`
Expected: All tests pass, no warnings

**Step 2: Fix any issues and commit**

---

## Task 23: Remove unsafe from AppController

**Files:**
- Modify: `crates/ambitious/src/application/core.rs`

**Step 1: Change type aliases from Box to Arc**

In `crates/ambitious/src/application/core.rs`, change lines 11-14 from:

```rust
type StartFn = Box<dyn Fn(&RuntimeHandle, &AppConfig) -> Result<StartResult, String> + Send + Sync>;

/// Type alias for the application stop function.
type StopFn = Box<dyn Fn(Option<Pid>) + Send + Sync>;
```

to:

```rust
type StartFn = Arc<dyn Fn(&RuntimeHandle, &AppConfig) -> Result<StartResult, String> + Send + Sync>;

/// Type alias for the application stop function.
type StopFn = Arc<dyn Fn(Option<Pid>) + Send + Sync>;
```

**Step 2: Update register to use Arc**

In the `register` method (line 106), change the function creation from `Box::new` to `Arc::new`:

```rust
let app = RegisteredApp {
    spec,
    start_fn: Arc::new(|handle, config| A::start(handle, config)),
    stop_fn: Arc::new(|pid| A::stop(pid)),
};
```

**Step 3: Replace unsafe block with safe Arc clone**

Replace the block at lines 149-172 (the `let (start_fn, _spec)` block and the unsafe call) with:

```rust
            let start_fn = {
                let registered = self.registered.read().unwrap();
                let app = registered
                    .get(&app_name)
                    .ok_or_else(|| StartError::NotFound(app_name.clone()))?;
                app.start_fn.clone()
            };

            // Start it
            let app_config = if app_name == name {
                config.clone()
            } else {
                AppConfig::new()
            };

            let result = start_fn(&self.handle, &app_config);
```

**Step 4: Add Arc import**

Check if `Arc` is already imported. Line 8 has:
```rust
use std::sync::{Arc, RwLock};
```

Already imported.

**Step 5: Run `cargo check`**

Run: `cargo check -p ambitious`
Expected: Compiles with no errors, no unsafe blocks

**Step 6: Run existing application tests**

Run: `cargo test -p ambitious application`
Expected: All tests pass

**Step 7: Commit**

```bash
git add crates/ambitious/src/application/core.rs
git commit -m "fix: remove unsafe from AppController by using Arc instead of Box"
```

---

## Task 24: Fix stop_all reverse dependency ordering

**Files:**
- Modify: `crates/ambitious/src/application/core.rs`

**Step 1: Replace stop_all implementation**

Replace the `stop_all` method (lines 218-229, now shifted) with:

```rust
    /// Stops all running applications in reverse dependency order.
    ///
    /// Applications that depend on others are stopped first, then their
    /// dependencies are stopped.
    pub async fn stop_all(&self) {
        // Collect all running app names and build dependency-aware stop order
        let stop_order = {
            let running = self.running.read().unwrap();
            let running_names: Vec<String> = running.keys().cloned().collect();

            if running_names.is_empty() {
                return;
            }

            // Build the full startup order for all running apps, then reverse it
            let mut startup_order = Vec::new();
            let mut visited = std::collections::HashSet::new();
            let registered = self.registered.read().unwrap();

            for name in &running_names {
                let mut path = Vec::new();
                // Ignore errors - just best-effort ordering
                let _ = Self::visit_deps(name, &registered, &mut startup_order, &mut visited, &mut path);
            }

            // Reverse the startup order for shutdown
            startup_order.into_iter().rev().collect::<Vec<String>>()
        };

        // Stop each in reverse dependency order
        for name in &stop_order {
            let _ = self.stop(name).await;
        }
    }
```

**Step 2: Run tests**

Run: `cargo test -p ambitious application`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/ambitious/src/application/core.rs
git commit -m "fix: stop_all uses reverse dependency order"
```

---

## Task 25: Run full CI for Phase 4

**Step 1: Run CI**

Run: `just ci`
Expected: All tests pass, no warnings

**Step 2: Fix any issues and commit**

---

## Task 26: Update CLAUDE.md documentation

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Update relevant sections**

Make the following updates to `CLAUDE.md`:

1. **Crate structure**: Note that the project is a single crate (not the multi-crate structure shown)

2. **Process API**: Add `link`, `unlink`, `monitor`, `demonitor`, `flag`, `send_after` to the process functions

3. **GenServer API**: Update to show:
   - `ServerRef` type and usage with `call`/`cast`
   - `reply()` public function
   - `stop()` public function
   - Current trait signature with `&mut self` handlers and `Reply<T>`/`Status` return types

4. **Supervisor API**: Add `terminate_child`, `delete_child`, `restart_child` as implemented

5. **Message trait**: Update to show `encode_local`/`decode_local`/`encode_remote`/`decode_remote` instead of `encode`/`decode`

**Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md to match current implementation"
```

---

## Task 27: Update README.md documentation

**Files:**
- Modify: `README.md`

**Step 1: Update GenServer example to match current API**

Fix the GenServer example to use:
- `Reply<T>` and `Status` return types
- `&mut self` handlers
- `ServerRef` usage example

**Step 2: Commit**

```bash
git add README.md
git commit -m "docs: update README.md examples to match current API"
```

---

## Task 28: Final CI run

**Step 1: Run full CI**

Run: `just ci`
Expected: All tests pass, no warnings, clean build

**Step 2: Verify the changes**

Run: `git log --oneline -20`
Verify all commits are present and well-described.

---

## Summary of Changes

| Phase | What | Files |
|-------|------|-------|
| 1 | Process convenience functions | `core/process_flag.rs`, `runtime/task_local.rs`, `runtime/mod.rs`, `lib.rs` |
| 2 | GenServer ServerRef + reply + stop | `gen_server/server_ref.rs`, `gen_server/server.rs`, `gen_server/mod.rs`, `lib.rs` |
| 3 | Supervisor commands | `supervisor/core.rs`, `supervisor/mod.rs` |
| 4 | Application safety + ordering | `application/core.rs` |
| 5 | Documentation sync | `CLAUDE.md`, `README.md` |
