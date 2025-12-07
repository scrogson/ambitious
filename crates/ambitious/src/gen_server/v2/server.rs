//! GenServer v2 server loop and start functions.

use super::dispatch;
use super::protocol::{self, From, Message as ProtocolMessage};
use super::traits::GenServer;
use super::types::{Init, Status};
use crate::core::{ExitReason, Pid, Ref, Term};
use crate::message::{self, Message};
use crate::process::global;
use std::time::Duration;

/// Error type for GenServer operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// GenServer init returned Ignore.
    #[error("init returned ignore")]
    Ignore,
    /// GenServer init returned Stop.
    #[error("init returned stop: {0:?}")]
    InitStop(ExitReason),
    /// GenServer is not running.
    #[error("process not found")]
    NotFound,
    /// Call timed out.
    #[error("call timed out")]
    Timeout,
    /// GenServer stopped during call.
    #[error("gen_server stopped: {0:?}")]
    Stopped(ExitReason),
    /// Decode error.
    #[error("decode error: {0}")]
    Decode(#[from] crate::core::DecodeError),
}

/// Start a GenServer process (not linked to caller).
///
/// Handlers can be registered using either:
/// - `#[call]`, `#[cast]`, `#[info]` attribute macros (recommended)
/// - `register_handlers!` macro (legacy)
///
/// Returns the Pid of the new process, or an error if init fails.
pub async fn start<G: GenServer>(args: G::Args) -> Result<Pid, Error> {
    start_impl::<G>(args, false).await
}

/// Start a GenServer process linked to the caller.
///
/// Handlers can be registered using either:
/// - `#[call]`, `#[cast]`, `#[info]` attribute macros (recommended)
/// - `register_handlers!` macro (legacy)
///
/// Returns the Pid of the new process, or an error if init fails.
/// If the GenServer crashes, the caller will also crash (unless trapping exits).
pub async fn start_link<G: GenServer>(args: G::Args) -> Result<Pid, Error> {
    start_impl::<G>(args, true).await
}

async fn start_impl<G: GenServer>(args: G::Args, link: bool) -> Result<Pid, Error> {
    // Channel to receive init result
    use tokio::sync::oneshot;
    let (init_tx, init_rx) = oneshot::channel();

    // Spawn the process
    let handle = global::handle();
    let caller_pid = crate::try_current_pid();

    let pid = if link {
        // Must have a caller pid to link
        let parent = caller_pid.ok_or_else(|| {
            Error::InitStop(ExitReason::Error(
                "start_link requires caller context".into(),
            ))
        })?;
        handle.spawn_link(parent, move || async move {
            gen_server_main::<G>(args, Some(init_tx)).await;
        })
    } else {
        handle.spawn(move || async move {
            gen_server_main::<G>(args, Some(init_tx)).await;
        })
    };

    // Wait for init result
    match init_rx.await {
        Ok(Ok(())) => Ok(pid),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::InitStop(ExitReason::Error(
            "init channel closed".into(),
        ))),
    }
}

/// Main entry point for a GenServer process.
async fn gen_server_main<G: GenServer>(
    args: G::Args,
    init_tx: Option<tokio::sync::oneshot::Sender<Result<(), Error>>>,
) {
    // Call init
    let init_result = G::init(args).await;

    match init_result {
        Init::Ok(server) => {
            // Signal success
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            // Run the main loop
            gen_server_loop(server).await;
        }
        Init::Continue(server, continue_arg) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            // Schedule continue message
            schedule_continue(&continue_arg);
            gen_server_loop(server).await;
        }
        Init::Timeout(server, duration) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            // Schedule timeout
            schedule_timeout(duration);
            gen_server_loop(server).await;
        }
        Init::Ignore => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Err(Error::Ignore));
            }
            // Process exits without running loop
        }
        Init::Stop(reason) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Err(Error::InitStop(reason)));
            }
            // Process exits without running loop
        }
    }
}

/// The main message loop for a GenServer.
///
/// Receives messages and dispatches to appropriate handlers.
///
/// Dispatch order:
/// 1. Try global registry (for `#[call]`/`#[cast]`/`#[info]` macros)
/// 2. Fall back to `GenServer::handle_call_raw` etc (for `register_handlers!` macro)
async fn gen_server_loop<G: GenServer>(mut server: G) {
    loop {
        // Receive next message
        let raw_msg = match crate::recv().await {
            Some(m) => m,
            None => {
                // Mailbox closed, terminate
                server.terminate(ExitReason::Normal).await;
                return;
            }
        };

        // Try to decode as protocol message
        let msg = match ProtocolMessage::decode(&raw_msg) {
            Ok(m) => m,
            Err(_) => {
                // Not a protocol message - treat as info
                // Try global registry first, then fall back to handle_info_raw
                let status = if let Ok((tag, payload)) = message::decode_tag(&raw_msg) {
                    if let Some(status) = dispatch::dispatch_info(&mut server, tag, payload).await {
                        status
                    } else {
                        // No handler in global registry, try handle_info_raw
                        server.handle_info_raw(raw_msg).await
                    }
                } else {
                    // Can't decode tag, try handle_info_raw directly
                    server.handle_info_raw(raw_msg).await
                };
                if let Some(reason) = handle_status(status) {
                    server.terminate(reason).await;
                    return;
                }
                continue;
            }
        };

        // Dispatch based on message type
        match msg {
            ProtocolMessage::Call { from, payload } => {
                // Try global registry first, then fall back to handle_call_raw
                let reply_result = if let Ok((tag, msg_payload)) = message::decode_tag(&payload) {
                    if let Some(reply) =
                        dispatch::dispatch_call(&mut server, tag, msg_payload, from.clone()).await
                    {
                        reply
                    } else {
                        // No handler in global registry, try handle_call_raw
                        server.handle_call_raw(payload, from.clone()).await
                    }
                } else {
                    RawReply::StopNoReply(ExitReason::Error("decode error".into()))
                };
                match reply_result {
                    RawReply::Ok(reply_bytes) => {
                        send_reply(&from, reply_bytes);
                    }
                    RawReply::Continue(reply_bytes, continue_arg) => {
                        send_reply(&from, reply_bytes);
                        schedule_continue(&continue_arg);
                    }
                    RawReply::Timeout(reply_bytes, duration) => {
                        send_reply(&from, reply_bytes);
                        schedule_timeout(duration);
                    }
                    RawReply::NoReply => {
                        // Caller will wait until reply() is called
                    }
                    RawReply::Stop(reason, reply_bytes) => {
                        send_reply(&from, reply_bytes);
                        server.terminate(reason).await;
                        return;
                    }
                    RawReply::StopNoReply(reason) => {
                        // Send error to waiting caller
                        send_error(&from, &reason);
                        server.terminate(reason).await;
                        return;
                    }
                }
            }
            ProtocolMessage::Cast { payload } => {
                // Try global registry first, then fall back to handle_cast_raw
                let status = if let Ok((tag, msg_payload)) = message::decode_tag(&payload) {
                    if let Some(status) =
                        dispatch::dispatch_cast(&mut server, tag, msg_payload).await
                    {
                        status
                    } else {
                        // No handler in global registry, try handle_cast_raw
                        server.handle_cast_raw(payload).await
                    }
                } else {
                    Status::Stop(ExitReason::Error("decode error".into()))
                };
                if let Some(reason) = handle_status(status) {
                    server.terminate(reason).await;
                    return;
                }
            }
            ProtocolMessage::Stop { reason, from } => {
                if let Some(f) = from {
                    // Reply with :ok before stopping
                    send_reply(&f, Term::encode(&()));
                }
                server.terminate(reason).await;
                return;
            }
            ProtocolMessage::Timeout => {
                let status = server.handle_timeout().await;
                if let Some(reason) = handle_status(status) {
                    server.terminate(reason).await;
                    return;
                }
            }
            ProtocolMessage::Continue { arg } => {
                let status = server.handle_continue(arg).await;
                if let Some(reason) = handle_status(status) {
                    server.terminate(reason).await;
                    return;
                }
            }
            ProtocolMessage::Reply { .. } => {
                // Replies are handled by the caller, not the server
                // This shouldn't happen, but ignore if it does
                tracing::warn!("gen_server_v2 received unexpected Reply message");
            }
        }
    }
}

/// Raw reply type from handle_call_raw.
#[derive(Debug)]
pub enum RawReply {
    /// Reply with encoded bytes.
    Ok(Vec<u8>),
    /// Reply and schedule continue.
    Continue(Vec<u8>, Vec<u8>),
    /// Reply and schedule timeout.
    Timeout(Vec<u8>, Duration),
    /// No reply yet.
    NoReply,
    /// Stop after replying.
    Stop(ExitReason, Vec<u8>),
    /// Stop without replying.
    StopNoReply(ExitReason),
}

/// Handle a Status result, returning Some(reason) if should stop.
fn handle_status(status: Status) -> Option<ExitReason> {
    match status {
        Status::Ok => None,
        Status::Continue(arg) => {
            schedule_continue(&arg);
            None
        }
        Status::Timeout(duration) => {
            schedule_timeout(duration);
            None
        }
        Status::Stop(reason) => Some(reason),
    }
}

/// Send a reply to a waiting caller.
fn send_reply(from: &From, reply_bytes: Vec<u8>) {
    let reply_msg = protocol::encode_reply(from.reference, &reply_bytes);
    let _ = crate::send_raw(from.pid, reply_msg);
}

/// Send an error reply to a waiting caller.
fn send_error(from: &From, reason: &ExitReason) {
    // Encode the error as a reply
    let error_bytes = Term::encode(&format!("stopped: {:?}", reason));
    let reply_msg = protocol::encode_reply(from.reference, &error_bytes);
    let _ = crate::send_raw(from.pid, reply_msg);
}

/// Schedule a timeout message to be sent after the given duration.
fn schedule_timeout(duration: Duration) {
    let pid = crate::current_pid();
    let timeout_msg = protocol::encode_timeout();
    tokio::spawn(async move {
        tokio::time::sleep(duration).await;
        let _ = crate::send_raw(pid, timeout_msg);
    });
}

/// Schedule a continue message to be sent immediately.
fn schedule_continue(arg: &[u8]) {
    let pid = crate::current_pid();
    let continue_msg = protocol::encode_continue(arg);
    // Send immediately (but async to not block)
    tokio::spawn(async move {
        let _ = crate::send_raw(pid, continue_msg);
    });
}

/// Send a reply to a From handle.
///
/// Use this when returning `Reply::NoReply` to send the reply later.
pub fn reply<T: Term>(from: &From, value: T) {
    let reply_bytes = Term::encode(&value);
    send_reply(from, reply_bytes);
}

/// Make a synchronous call to a GenServer using a typed Message.
///
/// Sends a call message and waits for the reply.
/// The message is encoded using `Message::encode_local()`.
pub async fn call<M: Message, R: Message>(
    pid: Pid,
    request: M,
    timeout: Duration,
) -> Result<R, Error> {
    call_raw(pid, request.encode_local(), timeout).await
}

/// Make a synchronous call with raw bytes (no encoding).
///
/// Use this when you've already encoded the payload.
pub async fn call_raw<R: Message>(
    pid: Pid,
    payload: Vec<u8>,
    timeout: Duration,
) -> Result<R, Error> {
    use tokio::time::timeout as tokio_timeout;

    // Create a unique reference for this call
    let reference = Ref::new();
    let caller_pid = crate::current_pid();

    // Build the From
    let from = From {
        pid: caller_pid,
        reference,
    };

    // Build and send the call message directly
    let call_msg = ProtocolMessage::Call { from, payload }.encode();
    let _ = crate::send_raw(pid, call_msg);

    // Wait for reply with timeout
    let result = tokio_timeout(timeout, wait_for_reply::<R>(reference)).await;

    match result {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Timeout),
    }
}

/// Wait for a reply matching the given reference.
async fn wait_for_reply<R: Message>(reference: Ref) -> Result<R, Error> {
    loop {
        let raw_msg = match crate::recv().await {
            Some(m) => m,
            None => return Err(Error::Stopped(ExitReason::Normal)),
        };

        // Try to decode as protocol message
        if let Ok(ProtocolMessage::Reply {
            reference: ref r,
            payload,
        }) = ProtocolMessage::decode(&raw_msg)
            && *r == reference
        {
            // The payload is encoded with encode_local() which includes a tag.
            // Strip the tag first, then decode the payload.
            let (_tag, reply_payload) = crate::message::decode_tag(&payload)?;
            let reply = R::decode_local(reply_payload)?;
            return Ok(reply);
        }

        // Not our reply - put it back? Or just drop it?
        // For now, we log and continue waiting
        tracing::trace!("call: ignoring non-reply message while waiting");
    }
}

/// Send an asynchronous cast to a GenServer using a typed Message.
///
/// Returns immediately without waiting for a response.
/// The message is encoded using `Message::encode_local()`.
pub fn cast<M: Message>(pid: Pid, msg: M) {
    cast_raw(pid, msg.encode_local());
}

/// Send an asynchronous cast with raw bytes (no encoding).
///
/// Use this when you've already encoded the payload.
pub fn cast_raw(pid: Pid, payload: Vec<u8>) {
    let cast_msg = ProtocolMessage::Cast { payload }.encode();
    let _ = crate::send_raw(pid, cast_msg);
}

/// Request a GenServer to stop.
pub async fn stop(pid: Pid, reason: ExitReason) -> Result<(), Error> {
    let reference = Ref::new();
    let caller_pid = crate::current_pid();

    let from = From {
        pid: caller_pid,
        reference,
    };

    let stop_msg = protocol::encode_stop(reason, Some(from));
    let _ = crate::send_raw(pid, stop_msg);

    // Wait for acknowledgment
    wait_for_reply::<()>(reference).await
}
