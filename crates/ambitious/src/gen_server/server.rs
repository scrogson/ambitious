//! GenServer trait and process loop implementation.

use super::error::{CallError, StartError, StopError};
use super::protocol::{self, GenServerMessage};
use super::types::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, InfoResult, InitResult, ServerRef,
};
use crate::core::{ExitReason, Pid, RawTerm, Ref, SystemMessage, Term};
use crate::runtime::current_pid;
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::oneshot;

/// The GenServer trait for implementing request/response servers.
///
/// This trait mirrors Elixir's GenServer callbacks. All callbacks are async.
/// Task-local functions like `ambitious::current_pid()`, `ambitious::recv()`, etc.
/// are available within callbacks. For runtime operations like spawning
/// processes, use `ambitious::handle()`.
///
/// # Type Parameters
///
/// - `State`: The server's internal state
/// - `InitArg`: Argument passed to `init`
/// - `Call`: Request type for synchronous calls
/// - `Cast`: Message type for asynchronous casts
/// - `Reply`: Response type for calls
///
/// # Example
///
/// ```ignore
/// use ambitious::gen_server::{GenServer, InitResult, CallResult, CastResult};
/// use async_trait::async_trait;
///
/// struct Counter;
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// enum CounterCall { Get, Increment }
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// enum CounterCast { Reset }
///
/// #[async_trait]
/// impl GenServer for Counter {
///     type State = i64;
///     type InitArg = i64;
///     type Call = CounterCall;
///     type Cast = CounterCast;
///     type Reply = i64;
///
///     async fn init(arg: i64) -> InitResult<i64> {
///         InitResult::ok(arg)
///     }
///
///     async fn handle_call(
///         request: CounterCall,
///         _from: From,
///         state: &mut i64,
///     ) -> CallResult<i64, i64> {
///         match request {
///             CounterCall::Get => CallResult::reply(*state, *state),
///             CounterCall::Increment => {
///                 *state += 1;
///                 CallResult::reply(*state, *state)
///             }
///         }
///     }
///
///     // ... other callbacks
/// }
/// ```
#[async_trait]
pub trait GenServer: Sized + Send + Sync + 'static {
    /// The server's state type.
    type State: Send + 'static;
    /// Argument passed to the `init` callback.
    type InitArg: Term;
    /// Request type for synchronous calls.
    type Call: Term;
    /// Message type for asynchronous casts.
    type Cast: Term;
    /// Reply type for calls.
    type Reply: Term;

    /// Initializes the server state.
    ///
    /// Called when the server starts. Use `ambitious::current_pid()` to get the
    /// server's PID, or other task-local functions as needed.
    async fn init(arg: Self::InitArg) -> InitResult<Self::State>;

    /// Handles a synchronous call.
    ///
    /// The `from` parameter can be used for deferred replies.
    async fn handle_call(
        request: Self::Call,
        from: From,
        state: &mut Self::State,
    ) -> CallResult<Self::State, Self::Reply>;

    /// Handles an asynchronous cast.
    async fn handle_cast(msg: Self::Cast, state: &mut Self::State) -> CastResult<Self::State>;

    /// Handles other messages (system messages, raw messages, etc.).
    ///
    /// The message is passed as a [`RawTerm`] which can be decoded using
    /// `msg.decode::<T>()` to try to deserialize as a specific type.
    async fn handle_info(msg: RawTerm, state: &mut Self::State) -> InfoResult<Self::State>;

    /// Handles continue instructions from init/call/cast results.
    async fn handle_continue(
        arg: ContinueArg,
        state: &mut Self::State,
    ) -> ContinueResult<Self::State>;

    /// Called when the server is about to terminate.
    ///
    /// The default implementation does nothing.
    async fn terminate(_reason: ExitReason, _state: &mut Self::State) {}
}

/// Starts a GenServer without linking to the caller.
///
/// Returns the PID of the started server.
pub async fn start<G: GenServer>(arg: G::InitArg) -> Result<Pid, StartError> {
    start_internal::<G>(arg, false, None).await
}

/// Starts a GenServer linked to the calling process.
///
/// Returns the PID of the started server.
pub async fn start_link<G: GenServer>(parent: Pid, arg: G::InitArg) -> Result<Pid, StartError> {
    start_internal::<G>(arg, true, Some(parent)).await
}

/// Internal start implementation.
async fn start_internal<G: GenServer>(
    arg: G::InitArg,
    link: bool,
    parent: Option<Pid>,
) -> Result<Pid, StartError> {
    let handle = crate::process::global::handle();

    // Channel for init result
    let (init_tx, init_rx) = oneshot::channel::<Result<(), StartError>>();

    let pid = if link {
        handle.spawn_link(parent.unwrap(), move || {
            gen_server_loop::<G>(arg, Some(init_tx))
        })
    } else {
        handle.spawn(move || gen_server_loop::<G>(arg, Some(init_tx)))
    };

    // Wait for init result
    match init_rx.await {
        Ok(Ok(())) => Ok(pid),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::SpawnFailed("init channel closed".to_string())),
    }
}

/// The main GenServer process loop.
async fn gen_server_loop<G: GenServer>(
    arg: G::InitArg,
    init_tx: Option<oneshot::Sender<Result<(), StartError>>>,
) {
    // Run init
    let mut state = match G::init(arg).await {
        InitResult::Ok(s) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            s
        }
        InitResult::OkTimeout(s, timeout) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            schedule_timeout(timeout);
            s
        }
        InitResult::OkHibernate(s) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            // Hibernate is a no-op for now
            s
        }
        InitResult::OkContinue(s, arg) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            schedule_continue(&arg);
            s
        }
        InitResult::Ignore => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Err(StartError::Ignore));
            }
            return;
        }
        InitResult::Stop(reason) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Err(StartError::Stop(reason)));
            }
            return;
        }
    };

    // Main message loop - use task-local recv
    loop {
        let msg = match crate::runtime::recv().await {
            Some(m) => m,
            None => {
                // Mailbox closed
                G::terminate(ExitReason::Normal, &mut state).await;
                return;
            }
        };

        // Try to decode as GenServer protocol message
        match protocol::decode(&msg) {
            Ok(GenServerMessage::Call { from, payload }) => {
                match <G::Call as Term>::decode(&payload) {
                    Ok(request) => {
                        let result = G::handle_call(request, from, &mut state).await;
                        match handle_call_result::<G>(result, from, &mut state) {
                            LoopAction::Continue => {}
                            LoopAction::ContinueTimeout(timeout) => {
                                schedule_timeout(timeout);
                            }
                            LoopAction::ContinueWith(arg) => {
                                schedule_continue(&arg);
                            }
                            LoopAction::Stop(reason) => {
                                G::terminate(reason, &mut state).await;
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        // Failed to decode call - ignore
                    }
                }
            }
            Ok(GenServerMessage::Cast { payload }) => {
                match <G::Cast as Term>::decode(&payload) {
                    Ok(cast_msg) => {
                        let result = G::handle_cast(cast_msg, &mut state).await;
                        match handle_cast_result::<G>(result, &mut state) {
                            LoopAction::Continue => {}
                            LoopAction::ContinueTimeout(timeout) => {
                                schedule_timeout(timeout);
                            }
                            LoopAction::ContinueWith(arg) => {
                                schedule_continue(&arg);
                            }
                            LoopAction::Stop(reason) => {
                                G::terminate(reason, &mut state).await;
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        // Failed to decode cast - ignore
                    }
                }
            }
            Ok(GenServerMessage::Reply { .. }) => {
                // Replies are handled by the calling process, not us
            }
            Ok(GenServerMessage::Stop { reason, from }) => {
                if let Some(f) = from {
                    // Reply with :ok before stopping
                    let reply_data = protocol::encode_reply(f.reference, &());
                    let _ = crate::runtime::send_raw(f.caller, reply_data);
                }
                G::terminate(reason, &mut state).await;
                return;
            }
            Ok(GenServerMessage::Timeout) => {
                // Synthesize a timeout info message
                let result = G::handle_info(protocol::encode_timeout().into(), &mut state).await;
                match handle_cast_result::<G>(result, &mut state) {
                    LoopAction::Continue => {}
                    LoopAction::ContinueTimeout(timeout) => {
                        schedule_timeout(timeout);
                    }
                    LoopAction::ContinueWith(arg) => {
                        schedule_continue(&arg);
                    }
                    LoopAction::Stop(reason) => {
                        G::terminate(reason, &mut state).await;
                        return;
                    }
                }
            }
            Ok(GenServerMessage::Continue { arg }) => {
                let continue_arg = ContinueArg(arg);
                let result = G::handle_continue(continue_arg, &mut state).await;
                match handle_cast_result::<G>(result, &mut state) {
                    LoopAction::Continue => {}
                    LoopAction::ContinueTimeout(timeout) => {
                        schedule_timeout(timeout);
                    }
                    LoopAction::ContinueWith(arg) => {
                        schedule_continue(&arg);
                    }
                    LoopAction::Stop(reason) => {
                        G::terminate(reason, &mut state).await;
                        return;
                    }
                }
            }
            Err(_) => {
                // Not a GenServer protocol message - treat as info
                // First check if it's a system message
                if let Ok(sys_msg) = <SystemMessage as Term>::decode(&msg) {
                    match sys_msg {
                        SystemMessage::Exit { from: _, reason } => {
                            // Exit signal received
                            G::terminate(reason, &mut state).await;
                            return;
                        }
                        SystemMessage::Down {
                            monitor_ref: _,
                            pid: _,
                            reason: _,
                        } => {
                            // Monitor down - pass to handle_info
                            let result = G::handle_info(msg.into(), &mut state).await;
                            match handle_cast_result::<G>(result, &mut state) {
                                LoopAction::Continue => {}
                                LoopAction::ContinueTimeout(timeout) => {
                                    schedule_timeout(timeout);
                                }
                                LoopAction::ContinueWith(arg) => {
                                    schedule_continue(&arg);
                                }
                                LoopAction::Stop(reason) => {
                                    G::terminate(reason, &mut state).await;
                                    return;
                                }
                            }
                        }
                        SystemMessage::Timeout => {
                            // System timeout
                            let result = G::handle_info(msg.into(), &mut state).await;
                            match handle_cast_result::<G>(result, &mut state) {
                                LoopAction::Continue => {}
                                LoopAction::ContinueTimeout(timeout) => {
                                    schedule_timeout(timeout);
                                }
                                LoopAction::ContinueWith(arg) => {
                                    schedule_continue(&arg);
                                }
                                LoopAction::Stop(reason) => {
                                    G::terminate(reason, &mut state).await;
                                    return;
                                }
                            }
                        }
                    }
                } else {
                    // Unknown message - pass to handle_info
                    let result = G::handle_info(msg.into(), &mut state).await;
                    match handle_cast_result::<G>(result, &mut state) {
                        LoopAction::Continue => {}
                        LoopAction::ContinueTimeout(timeout) => {
                            schedule_timeout(timeout);
                        }
                        LoopAction::ContinueWith(arg) => {
                            schedule_continue(&arg);
                        }
                        LoopAction::Stop(reason) => {
                            G::terminate(reason, &mut state).await;
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Actions the loop can take after handling a message.
enum LoopAction {
    Continue,
    ContinueTimeout(Duration),
    ContinueWith(ContinueArg),
    Stop(ExitReason),
}

/// Handles a CallResult and returns the appropriate loop action.
fn handle_call_result<G: GenServer>(
    result: CallResult<G::State, G::Reply>,
    from: From,
    state: &mut G::State,
) -> LoopAction {
    match result {
        CallResult::Reply(reply, new_state) => {
            send_reply::<G>(from, &reply);
            *state = new_state;
            LoopAction::Continue
        }
        CallResult::ReplyTimeout(reply, new_state, timeout) => {
            send_reply::<G>(from, &reply);
            *state = new_state;
            LoopAction::ContinueTimeout(timeout)
        }
        CallResult::ReplyContinue(reply, new_state, arg) => {
            send_reply::<G>(from, &reply);
            *state = new_state;
            LoopAction::ContinueWith(arg)
        }
        CallResult::NoReply(new_state) => {
            *state = new_state;
            LoopAction::Continue
        }
        CallResult::NoReplyTimeout(new_state, timeout) => {
            *state = new_state;
            LoopAction::ContinueTimeout(timeout)
        }
        CallResult::NoReplyContinue(new_state, arg) => {
            *state = new_state;
            LoopAction::ContinueWith(arg)
        }
        CallResult::Stop(reason, reply, new_state) => {
            send_reply::<G>(from, &reply);
            *state = new_state;
            LoopAction::Stop(reason)
        }
        CallResult::StopNoReply(reason, new_state) => {
            *state = new_state;
            LoopAction::Stop(reason)
        }
    }
}

/// Handles a CastResult and returns the appropriate loop action.
fn handle_cast_result<G: GenServer>(
    result: CastResult<G::State>,
    state: &mut G::State,
) -> LoopAction {
    match result {
        CastResult::NoReply(new_state) => {
            *state = new_state;
            LoopAction::Continue
        }
        CastResult::NoReplyTimeout(new_state, timeout) => {
            *state = new_state;
            LoopAction::ContinueTimeout(timeout)
        }
        CastResult::NoReplyContinue(new_state, arg) => {
            *state = new_state;
            LoopAction::ContinueWith(arg)
        }
        CastResult::Stop(reason, new_state) => {
            *state = new_state;
            LoopAction::Stop(reason)
        }
    }
}

/// Sends a reply to a caller.
fn send_reply<G: GenServer>(from: From, reply: &G::Reply) {
    let reply_data = protocol::encode_reply(from.reference, reply);
    let _ = crate::runtime::send_raw(from.caller, reply_data);
}

/// Schedules a timeout message.
fn schedule_timeout(timeout: Duration) {
    let pid = current_pid();
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        let data = protocol::encode_timeout();
        let handle = crate::process::global::handle();
        let _ = handle.registry().send_raw(pid, data);
    });
}

/// Schedules a continue message.
fn schedule_continue(arg: &ContinueArg) {
    let pid = current_pid();
    let data = protocol::encode_continue(&arg.0);
    let _ = crate::runtime::send_raw(pid, data);
}

/// Makes a synchronous call to a GenServer.
///
/// Waits for the reply with the given timeout. Uses task-local context.
/// Only works from within a Ambitious process.
///
/// Note: This function implements selective receive - it will loop through
/// incoming messages looking for the matching reply, re-sending any non-matching
/// messages back to itself to preserve them for later processing.
pub async fn call<G: GenServer>(
    server: impl Into<ServerRef>,
    request: G::Call,
    timeout: Duration,
) -> Result<G::Reply, CallError> {
    let server_pid = resolve_server(server)?;
    let self_pid = current_pid();

    let reference = Ref::new();
    let from = From::new(self_pid, reference);

    // Send the call using task-local send
    let call_data = protocol::encode_call(from, &request);
    crate::runtime::send_raw(server_pid, call_data).map_err(|_| CallError::NotAlive(server_pid))?;

    // Selective receive: loop until we get the matching reply or timeout
    let start = std::time::Instant::now();
    let mut stashed_messages: Vec<Vec<u8>> = Vec::new();

    loop {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            // Re-send stashed messages back to ourselves before returning
            resend_stashed(self_pid, stashed_messages);
            return Err(CallError::Timeout);
        }

        match crate::runtime::recv_timeout(remaining).await {
            Ok(Some(reply_data)) => {
                // Try to decode as a GenServer reply
                if let Ok(GenServerMessage::Reply {
                    reference: ref_,
                    payload,
                }) = protocol::decode(&reply_data)
                {
                    if ref_ == reference {
                        // Found our reply! Re-send stashed messages before returning
                        resend_stashed(self_pid, stashed_messages);
                        return <G::Reply as Term>::decode(&payload)
                            .map_err(|_| CallError::Exit(ExitReason::error("decode error")));
                    } else {
                        // Reply for a different call - stash it
                        stashed_messages.push(reply_data);
                    }
                } else {
                    // Not a GenServer reply - stash it for later processing
                    stashed_messages.push(reply_data);
                }
            }
            Ok(None) => {
                // Mailbox closed - this shouldn't happen for a live process
                // This can occur if all senders (ProcessHandle) are dropped
                resend_stashed(self_pid, stashed_messages);
                return Err(CallError::Exit(ExitReason::Normal));
            }
            Err(()) => {
                // Timeout on recv - but this is within the overall call timeout
                // Keep looping until the call timeout expires
                continue;
            }
        }
    }
}

/// Re-send stashed messages back to the process using the global handle.
fn resend_stashed(pid: Pid, messages: Vec<Vec<u8>>) {
    let handle = crate::process::global::handle();
    for msg in messages {
        let _ = handle.registry().send_raw(pid, msg);
    }
}

/// Sends an asynchronous cast to a GenServer.
pub fn cast<G: GenServer>(server: impl Into<ServerRef>, msg: G::Cast) -> Result<(), CallError> {
    let handle = crate::process::global::handle();
    let server_pid = resolve_server(server)?;

    let cast_data = protocol::encode_cast(&msg);
    handle
        .registry()
        .send_raw(server_pid, cast_data)
        .map_err(|_| CallError::NotAlive(server_pid))
}

/// Sends a reply to a pending call.
///
/// This is used for deferred replies when `handle_call` returns `NoReply`.
pub fn reply<R: Term>(from: From, reply: R) -> Result<(), CallError> {
    let handle = crate::process::global::handle();
    let reply_data = protocol::encode_reply(from.reference, &reply);
    handle
        .registry()
        .send_raw(from.caller, reply_data)
        .map_err(|_| CallError::NotAlive(from.caller))
}

/// Stops a GenServer.
///
/// Uses task-local context. Only works from within a Ambitious process.
pub async fn stop(
    server: impl Into<ServerRef>,
    reason: ExitReason,
    timeout: Duration,
) -> Result<(), StopError> {
    let server_pid = resolve_server_stop(server)?;
    let self_pid = current_pid();

    let reference = Ref::new();
    let from = From::new(self_pid, reference);

    // Send stop message using task-local send
    let stop_data = protocol::encode_stop(reason, Some(from));
    crate::runtime::send_raw(server_pid, stop_data).map_err(|_| StopError::NotAlive(server_pid))?;

    // Wait for acknowledgment
    match crate::runtime::recv_timeout(timeout).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Ok(()), // Server stopped
        Err(()) => Err(StopError::Timeout),
    }
}

/// Resolves a ServerRef to a Pid.
fn resolve_server(server: impl Into<ServerRef>) -> Result<Pid, CallError> {
    let server_ref = server.into();
    server_ref
        .resolve()
        .ok_or_else(|| CallError::NotFound(format!("{:?}", server_ref)))
}

/// Resolves a ServerRef to a Pid for stop operations.
fn resolve_server_stop(server: impl Into<ServerRef>) -> Result<Pid, StopError> {
    let server_ref = server.into();
    server_ref
        .resolve()
        .ok_or_else(|| StopError::NotFound(format!("{:?}", server_ref)))
}
