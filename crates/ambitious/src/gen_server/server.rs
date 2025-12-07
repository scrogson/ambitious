//! GenServer server loop and client API.
//!
//! This module provides:
//! - `start` / `start_link` - spawn a GenServer process
//! - `call` / `cast` - client functions using OTP envelope format
//! - The main message loop with envelope-based dispatch

use super::protocol::{
    self, From, Message as ProtocolMessage, decode_gen_call_envelope, decode_gen_cast_envelope,
    encode_gen_call_envelope, encode_gen_cast_envelope, encode_reply,
};
use super::traits::GenServer;
use super::types::{Init, Reply, Status};
use crate::core::{DecodeError, ExitReason, Pid, Ref, Term};
use crate::message::Message;
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
    Decode(#[from] DecodeError),
}

/// Start a GenServer process (not linked to caller).
///
/// # Example
///
/// ```ignore
/// let pid = start::<Counter>(42).await?;
/// ```
pub async fn start<G: GenServer>(args: G::Args) -> Result<Pid, Error> {
    start_impl::<G>(args, false).await
}

/// Start a GenServer process linked to the caller.
///
/// If the GenServer crashes, the caller will also crash (unless trapping exits).
pub async fn start_link<G: GenServer>(args: G::Args) -> Result<Pid, Error> {
    start_impl::<G>(args, true).await
}

async fn start_impl<G: GenServer>(args: G::Args, link: bool) -> Result<Pid, Error> {
    use tokio::sync::oneshot;
    let (init_tx, init_rx) = oneshot::channel();

    let handle = global::handle();
    let caller_pid = crate::try_current_pid();

    let pid = if link {
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
    let init_result = G::init(args).await;

    match init_result {
        Init::Ok(server) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            gen_server_loop(server).await;
        }
        Init::Continue(server, continue_arg) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            schedule_continue(&continue_arg);
            gen_server_loop(server).await;
        }
        Init::Timeout(server, duration) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Ok(()));
            }
            schedule_timeout(duration);
            gen_server_loop(server).await;
        }
        Init::Ignore => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Err(Error::Ignore));
            }
        }
        Init::Stop(reason) => {
            if let Some(tx) = init_tx {
                let _ = tx.send(Err(Error::InitStop(reason)));
            }
        }
    }
}

/// The main message loop for a GenServer.
///
/// Uses OTP envelope format for dispatch:
/// - `$gen_call` envelope -> `handle_call`
/// - `$gen_cast` envelope -> `handle_cast`
/// - Other messages -> `handle_info`
async fn gen_server_loop<G: GenServer>(mut server: G) {
    loop {
        let raw_msg = match crate::recv().await {
            Some(m) => m,
            None => {
                server.terminate(ExitReason::Normal).await;
                return;
            }
        };

        // Try to decode as OTP envelope first
        if let Some(call_envelope) = decode_gen_call_envelope(&raw_msg) {
            // The request is encoded with encode_local() which includes a tag.
            // Strip the tag first, then decode.
            match crate::message::decode_tag(&call_envelope.request) {
                Ok((_tag, payload)) => match G::Call::decode_local(payload) {
                    Ok(msg) => {
                        let reply = server.handle_call(msg, call_envelope.from.clone()).await;
                        if let Some(reason) = handle_call_reply(&call_envelope.from, reply) {
                            server.terminate(reason).await;
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "Failed to decode call message");
                        send_error(&call_envelope.from, &ExitReason::Error(e.to_string()));
                    }
                },
                Err(e) => {
                    tracing::error!(error = ?e, "Failed to decode call message tag");
                    send_error(&call_envelope.from, &ExitReason::Error(e.to_string()));
                }
            }
            continue;
        }

        if let Some(cast_envelope) = decode_gen_cast_envelope(&raw_msg) {
            // The request is encoded with encode_local() which includes a tag.
            // Strip the tag first, then decode.
            match crate::message::decode_tag(&cast_envelope.request) {
                Ok((_tag, payload)) => match G::Cast::decode_local(payload) {
                    Ok(msg) => {
                        let status = server.handle_cast(msg).await;
                        if let Some(reason) = handle_status(status) {
                            server.terminate(reason).await;
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "Failed to decode cast message");
                    }
                },
                Err(e) => {
                    tracing::error!(error = ?e, "Failed to decode cast message tag");
                }
            }
            continue;
        }

        // Try to decode as legacy protocol message (for backwards compatibility)
        if let Ok(protocol_msg) = ProtocolMessage::decode(&raw_msg) {
            match protocol_msg {
                ProtocolMessage::Stop { reason, from } => {
                    if let Some(f) = from {
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
                    tracing::warn!("gen_server received unexpected Reply message");
                }
                // Legacy Call/Cast - try to decode as info
                _ => match G::Info::decode_local(&raw_msg) {
                    Ok(msg) => {
                        let status = server.handle_info(msg).await;
                        if let Some(reason) = handle_status(status) {
                            server.terminate(reason).await;
                            return;
                        }
                    }
                    Err(_) => {
                        tracing::trace!("Ignoring unknown message");
                    }
                },
            }
            continue;
        }

        // Not an envelope or protocol message - treat as info
        match G::Info::decode_local(&raw_msg) {
            Ok(msg) => {
                let status = server.handle_info(msg).await;
                if let Some(reason) = handle_status(status) {
                    server.terminate(reason).await;
                    return;
                }
            }
            Err(_) => {
                tracing::trace!("Ignoring unknown message");
            }
        }
    }
}

/// Handle a Reply result from handle_call, sending the reply to the caller.
/// Returns Some(reason) if the server should stop.
fn handle_call_reply<T: Message>(from: &From, reply: Reply<T>) -> Option<ExitReason> {
    match reply {
        Reply::Ok(value) => {
            send_reply(from, value.encode_local());
            None
        }
        Reply::Continue(value, continue_arg) => {
            send_reply(from, value.encode_local());
            schedule_continue(&continue_arg);
            None
        }
        Reply::Timeout(value, duration) => {
            send_reply(from, value.encode_local());
            schedule_timeout(duration);
            None
        }
        Reply::NoReply => None,
        Reply::Stop(reason, value) => {
            send_reply(from, value.encode_local());
            Some(reason)
        }
        Reply::StopNoReply(reason) => {
            send_error(from, &reason);
            Some(reason)
        }
    }
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
    let reply_msg = encode_reply(from.reference, &reply_bytes);
    let _ = crate::send_raw(from.pid, reply_msg);
}

/// Send an error reply to a waiting caller.
fn send_error(from: &From, reason: &ExitReason) {
    let error_bytes = Term::encode(&format!("stopped: {:?}", reason));
    let reply_msg = encode_reply(from.reference, &error_bytes);
    let _ = crate::send_raw(from.pid, reply_msg);
}

/// Schedule a timeout message to be sent after the given duration.
fn schedule_timeout(duration: Duration) {
    let pid = crate::current_pid();
    let timeout_msg = protocol::encode_timeout();
    tokio::spawn(async move {
        tokio::time::sleep(duration).await;
        // Use global runtime registry since this runs outside process context
        if let Some(handle) = crate::process::global::try_handle() {
            let _ = handle.registry().send_raw(pid, timeout_msg);
        }
    });
}

/// Schedule a continue message to be sent immediately.
fn schedule_continue(arg: &[u8]) {
    let pid = crate::current_pid();
    let continue_msg = protocol::encode_continue(arg);
    tokio::spawn(async move {
        // Use global runtime registry since this runs outside process context
        if let Some(handle) = crate::process::global::try_handle() {
            let _ = handle.registry().send_raw(pid, continue_msg);
        }
    });
}

// =============================================================================
// Client API
// =============================================================================

/// Make a synchronous call to a GenServer.
///
/// The message is wrapped in a `$gen_call` OTP envelope.
///
/// # Example
///
/// ```ignore
/// let count: i64 = call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5)).await?;
/// ```
pub async fn call<G, R>(pid: Pid, msg: G::Call, timeout: Duration) -> Result<R, Error>
where
    G: GenServer,
    R: Message,
{
    use tokio::time::timeout as tokio_timeout;

    let reference = Ref::new();
    let caller_pid = crate::current_pid();

    let from = From {
        pid: caller_pid,
        reference,
    };

    // Wrap in $gen_call envelope
    let envelope = encode_gen_call_envelope(from, msg.encode_local());
    let _ = crate::send_raw(pid, envelope);

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
            // The payload is encoded with encode_local() which already has the tag.
            // decode_local expects just the payload without tag, so we need to strip it.
            let (_tag, reply_payload) = crate::message::decode_tag(&payload)?;
            // Now reply_payload is the actual serialized data
            let reply = R::decode_local(reply_payload)?;
            return Ok(reply);
        }

        // Not our reply - log and continue waiting
        tracing::trace!("call: ignoring non-reply message while waiting");
    }
}

/// Send an asynchronous cast to a GenServer.
///
/// The message is wrapped in a `$gen_cast` OTP envelope.
/// Returns immediately without waiting for a response.
///
/// # Example
///
/// ```ignore
/// cast::<Counter>(pid, CounterCast::Increment);
/// ```
pub fn cast<G>(pid: Pid, msg: G::Cast)
where
    G: GenServer,
{
    // Wrap in $gen_cast envelope
    let envelope = encode_gen_cast_envelope(msg.encode_local());
    let _ = crate::send_raw(pid, envelope);
}
