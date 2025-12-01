//! GenServer trait and process loop implementation.

use crate::error::{CallError, StartError, StopError};
use crate::protocol::{self, GenServerMessage};
use crate::types::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, InfoResult, InitResult, ServerRef,
};
use dream_core::{ExitReason, Message, Pid, Ref, SystemMessage};
use dream_process::RuntimeHandle;
use dream_runtime::Context;
use std::time::Duration;
use tokio::sync::oneshot;

/// The GenServer trait for implementing request/response servers.
///
/// This trait mirrors Elixir's GenServer callbacks.
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
/// use dream_gen_server::{GenServer, InitResult, CallResult, CastResult};
///
/// struct Counter;
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// enum CounterCall {
///     Get,
///     Increment,
/// }
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// enum CounterCast {
///     Reset,
/// }
///
/// impl GenServer for Counter {
///     type State = i64;
///     type InitArg = i64;
///     type Call = CounterCall;
///     type Cast = CounterCast;
///     type Reply = i64;
///
///     fn init(initial: i64) -> InitResult<i64> {
///         InitResult::ok(initial)
///     }
///
///     fn handle_call(
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
///     fn handle_cast(msg: CounterCast, state: &mut i64) -> CastResult<i64> {
///         match msg {
///             CounterCast::Reset => CastResult::noreply(0),
///         }
///     }
/// }
/// ```
pub trait GenServer: Sized + Send + 'static {
    /// The server's state type.
    type State: Send + 'static;
    /// Argument passed to the `init` callback.
    type InitArg: Message;
    /// Request type for synchronous calls.
    type Call: Message;
    /// Message type for asynchronous casts.
    type Cast: Message;
    /// Reply type for calls.
    type Reply: Message;

    /// Initializes the server state.
    ///
    /// This is called when the server starts.
    fn init(arg: Self::InitArg) -> InitResult<Self::State>;

    /// Handles a synchronous call.
    ///
    /// The `from` parameter can be used for deferred replies.
    fn handle_call(
        request: Self::Call,
        from: From,
        state: &mut Self::State,
    ) -> CallResult<Self::State, Self::Reply>;

    /// Handles an asynchronous cast.
    fn handle_cast(msg: Self::Cast, state: &mut Self::State) -> CastResult<Self::State>;

    /// Handles other messages (system messages, raw messages, etc.).
    ///
    /// This callback must be implemented to handle non-call/cast messages.
    fn handle_info(msg: Vec<u8>, state: &mut Self::State) -> InfoResult<Self::State>;

    /// Handles continue instructions from init/call/cast results.
    ///
    /// This callback must be implemented if you use continue results.
    fn handle_continue(arg: ContinueArg, state: &mut Self::State) -> ContinueResult<Self::State>;

    /// Called when the server is about to terminate.
    ///
    /// The default implementation does nothing.
    #[allow(unused_variables)]
    fn terminate(reason: ExitReason, state: &mut Self::State) {}
}

/// Starts a GenServer without linking to the caller.
///
/// Returns the PID of the started server.
pub async fn start<G: GenServer>(
    handle: &RuntimeHandle,
    arg: G::InitArg,
) -> Result<Pid, StartError> {
    start_internal::<G>(handle, arg, false, None).await
}

/// Starts a GenServer linked to the calling process.
///
/// Returns the PID of the started server.
pub async fn start_link<G: GenServer>(
    handle: &RuntimeHandle,
    parent: Pid,
    arg: G::InitArg,
) -> Result<Pid, StartError> {
    start_internal::<G>(handle, arg, true, Some(parent)).await
}

/// Internal start implementation.
async fn start_internal<G: GenServer>(
    handle: &RuntimeHandle,
    arg: G::InitArg,
    link: bool,
    parent: Option<Pid>,
) -> Result<Pid, StartError> {
    // Channel for init result
    let (init_tx, init_rx) = oneshot::channel::<Result<(), StartError>>();

    let pid = if link {
        handle.spawn_link(parent.unwrap(), move |ctx| {
            gen_server_loop::<G>(ctx, arg, Some(init_tx))
        })
    } else {
        handle.spawn(move |ctx| gen_server_loop::<G>(ctx, arg, Some(init_tx)))
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
    mut ctx: Context,
    arg: G::InitArg,
    init_tx: Option<oneshot::Sender<Result<(), StartError>>>,
) {
    // Run init
    let mut state = match G::init(arg) {
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
            schedule_timeout(&ctx, timeout);
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
            schedule_continue(&ctx, &arg);
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

    // Main message loop
    loop {
        let msg = match ctx.recv().await {
            Some(m) => m,
            None => {
                // Mailbox closed
                G::terminate(ExitReason::Normal, &mut state);
                return;
            }
        };

        // Try to decode as GenServer protocol message
        match protocol::decode(&msg) {
            Ok(GenServerMessage::Call { from, payload }) => {
                match <G::Call as Message>::decode(&payload) {
                    Ok(request) => {
                        let result = G::handle_call(request, from, &mut state);
                        match handle_call_result::<G>(&ctx, result, from, &mut state) {
                            LoopAction::Continue => {}
                            LoopAction::ContinueTimeout(timeout) => {
                                schedule_timeout(&ctx, timeout);
                            }
                            LoopAction::ContinueWith(arg) => {
                                schedule_continue(&ctx, &arg);
                            }
                            LoopAction::Stop(reason) => {
                                G::terminate(reason, &mut state);
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
                match <G::Cast as Message>::decode(&payload) {
                    Ok(cast_msg) => {
                        let result = G::handle_cast(cast_msg, &mut state);
                        match handle_cast_result::<G>(result, &mut state) {
                            LoopAction::Continue => {}
                            LoopAction::ContinueTimeout(timeout) => {
                                schedule_timeout(&ctx, timeout);
                            }
                            LoopAction::ContinueWith(arg) => {
                                schedule_continue(&ctx, &arg);
                            }
                            LoopAction::Stop(reason) => {
                                G::terminate(reason, &mut state);
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
                    let _ = ctx.send_raw(f.caller, reply_data);
                }
                G::terminate(reason, &mut state);
                return;
            }
            Ok(GenServerMessage::Timeout) => {
                // Synthesize a timeout info message
                let result = G::handle_info(protocol::encode_timeout(), &mut state);
                match handle_cast_result::<G>(result, &mut state) {
                    LoopAction::Continue => {}
                    LoopAction::ContinueTimeout(timeout) => {
                        schedule_timeout(&ctx, timeout);
                    }
                    LoopAction::ContinueWith(arg) => {
                        schedule_continue(&ctx, &arg);
                    }
                    LoopAction::Stop(reason) => {
                        G::terminate(reason, &mut state);
                        return;
                    }
                }
            }
            Ok(GenServerMessage::Continue { arg }) => {
                let continue_arg = ContinueArg(arg);
                let result = G::handle_continue(continue_arg, &mut state);
                match handle_cast_result::<G>(result, &mut state) {
                    LoopAction::Continue => {}
                    LoopAction::ContinueTimeout(timeout) => {
                        schedule_timeout(&ctx, timeout);
                    }
                    LoopAction::ContinueWith(arg) => {
                        schedule_continue(&ctx, &arg);
                    }
                    LoopAction::Stop(reason) => {
                        G::terminate(reason, &mut state);
                        return;
                    }
                }
            }
            Err(_) => {
                // Not a GenServer protocol message - treat as info
                // First check if it's a system message
                if let Ok(sys_msg) = <SystemMessage as Message>::decode(&msg) {
                    match sys_msg {
                        SystemMessage::Exit { from: _, reason } => {
                            // Exit signal received
                            G::terminate(reason, &mut state);
                            return;
                        }
                        SystemMessage::Down {
                            monitor_ref: _,
                            pid: _,
                            reason: _,
                        } => {
                            // Monitor down - pass to handle_info
                            let result = G::handle_info(msg, &mut state);
                            match handle_cast_result::<G>(result, &mut state) {
                                LoopAction::Continue => {}
                                LoopAction::ContinueTimeout(timeout) => {
                                    schedule_timeout(&ctx, timeout);
                                }
                                LoopAction::ContinueWith(arg) => {
                                    schedule_continue(&ctx, &arg);
                                }
                                LoopAction::Stop(reason) => {
                                    G::terminate(reason, &mut state);
                                    return;
                                }
                            }
                        }
                        SystemMessage::Timeout => {
                            // System timeout
                            let result = G::handle_info(msg, &mut state);
                            match handle_cast_result::<G>(result, &mut state) {
                                LoopAction::Continue => {}
                                LoopAction::ContinueTimeout(timeout) => {
                                    schedule_timeout(&ctx, timeout);
                                }
                                LoopAction::ContinueWith(arg) => {
                                    schedule_continue(&ctx, &arg);
                                }
                                LoopAction::Stop(reason) => {
                                    G::terminate(reason, &mut state);
                                    return;
                                }
                            }
                        }
                    }
                } else {
                    // Unknown message - pass to handle_info
                    let result = G::handle_info(msg, &mut state);
                    match handle_cast_result::<G>(result, &mut state) {
                        LoopAction::Continue => {}
                        LoopAction::ContinueTimeout(timeout) => {
                            schedule_timeout(&ctx, timeout);
                        }
                        LoopAction::ContinueWith(arg) => {
                            schedule_continue(&ctx, &arg);
                        }
                        LoopAction::Stop(reason) => {
                            G::terminate(reason, &mut state);
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
    ctx: &Context,
    result: CallResult<G::State, G::Reply>,
    from: From,
    state: &mut G::State,
) -> LoopAction {
    match result {
        CallResult::Reply(reply, new_state) => {
            send_reply::<G>(ctx, from, &reply);
            *state = new_state;
            LoopAction::Continue
        }
        CallResult::ReplyTimeout(reply, new_state, timeout) => {
            send_reply::<G>(ctx, from, &reply);
            *state = new_state;
            LoopAction::ContinueTimeout(timeout)
        }
        CallResult::ReplyContinue(reply, new_state, arg) => {
            send_reply::<G>(ctx, from, &reply);
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
            send_reply::<G>(ctx, from, &reply);
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
fn send_reply<G: GenServer>(ctx: &Context, from: From, reply: &G::Reply) {
    let reply_data = protocol::encode_reply(from.reference, reply);
    let _ = ctx.send_raw(from.caller, reply_data);
}

/// Schedules a timeout message.
fn schedule_timeout(_ctx: &Context, timeout: Duration) {
    // TODO: Implement timeout scheduling properly
    // This requires access to the registry to send back to the process
    // For now, timeouts are scheduled but never delivered
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        // TODO: Actually send timeout to the process
        // This requires access to the registry which we don't have here
    });
}

/// Schedules a continue message.
fn schedule_continue(ctx: &Context, arg: &ContinueArg) {
    let data = protocol::encode_continue(&arg.0);
    let _ = ctx.send_raw(ctx.pid(), data);
}

/// Makes a synchronous call to a GenServer.
///
/// Waits for the reply with the given timeout.
pub async fn call<G: GenServer>(
    handle: &RuntimeHandle,
    caller_ctx: &mut Context,
    server: impl Into<ServerRef>,
    request: G::Call,
    timeout: Duration,
) -> Result<G::Reply, CallError> {
    let server_pid = resolve_server(handle, server)?;

    let reference = Ref::new();
    let from = From::new(caller_ctx.pid(), reference);

    // Send the call
    let call_data = protocol::encode_call(from, &request);
    caller_ctx
        .send_raw(server_pid, call_data)
        .map_err(|_| CallError::NotAlive(server_pid))?;

    // Wait for reply
    match caller_ctx.recv_timeout(timeout).await {
        Ok(Some(reply_data)) => {
            // Try to decode as a reply
            if let Ok(GenServerMessage::Reply {
                reference: ref_,
                payload,
            }) = protocol::decode(&reply_data)
            {
                if ref_ == reference {
                    <G::Reply as Message>::decode(&payload)
                        .map_err(|_| CallError::Exit(ExitReason::error("decode error")))
                } else {
                    Err(CallError::Exit(ExitReason::error("reference mismatch")))
                }
            } else {
                Err(CallError::Exit(ExitReason::error("unexpected message")))
            }
        }
        Ok(None) => Err(CallError::Exit(ExitReason::Normal)),
        Err(()) => Err(CallError::Timeout),
    }
}

/// Sends an asynchronous cast to a GenServer.
pub fn cast<G: GenServer>(
    handle: &RuntimeHandle,
    server: impl Into<ServerRef>,
    msg: G::Cast,
) -> Result<(), CallError> {
    let server_pid = resolve_server(handle, server)?;

    let cast_data = protocol::encode_cast(&msg);
    handle
        .registry()
        .send_raw(server_pid, cast_data)
        .map_err(|_| CallError::NotAlive(server_pid))
}

/// Sends a reply to a pending call.
///
/// This is used for deferred replies when `handle_call` returns `NoReply`.
pub fn reply<R: Message>(handle: &RuntimeHandle, from: From, reply: R) -> Result<(), CallError> {
    let reply_data = protocol::encode_reply(from.reference, &reply);
    handle
        .registry()
        .send_raw(from.caller, reply_data)
        .map_err(|_| CallError::NotAlive(from.caller))
}

/// Stops a GenServer.
pub async fn stop(
    handle: &RuntimeHandle,
    caller_ctx: &mut Context,
    server: impl Into<ServerRef>,
    reason: ExitReason,
    timeout: Duration,
) -> Result<(), StopError> {
    let server_pid = resolve_server_stop(handle, server)?;

    let reference = Ref::new();
    let from = From::new(caller_ctx.pid(), reference);

    // Send stop message
    let stop_data = protocol::encode_stop(reason, Some(from));
    caller_ctx
        .send_raw(server_pid, stop_data)
        .map_err(|_| StopError::NotAlive(server_pid))?;

    // Wait for acknowledgment
    match caller_ctx.recv_timeout(timeout).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Ok(()), // Server stopped
        Err(()) => Err(StopError::Timeout),
    }
}

/// Resolves a ServerRef to a Pid.
fn resolve_server(handle: &RuntimeHandle, server: impl Into<ServerRef>) -> Result<Pid, CallError> {
    match server.into() {
        ServerRef::Pid(pid) => Ok(pid),
        ServerRef::Name(name) => handle
            .registry()
            .whereis(&name)
            .ok_or_else(|| CallError::NotFound(name)),
    }
}

/// Resolves a ServerRef to a Pid for stop operations.
fn resolve_server_stop(
    handle: &RuntimeHandle,
    server: impl Into<ServerRef>,
) -> Result<Pid, StopError> {
    match server.into() {
        ServerRef::Pid(pid) => Ok(pid),
        ServerRef::Name(name) => handle
            .registry()
            .whereis(&name)
            .ok_or_else(|| StopError::NotFound(name)),
    }
}
