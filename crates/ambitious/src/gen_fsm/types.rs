//! GenFsm types and result enums.

use crate::core::{ExitReason, Pid, Ref};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A handle identifying a pending call that needs a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct From {
    /// The PID of the calling process.
    pub caller: Pid,
    /// The unique reference for this call.
    pub reference: Ref,
}

impl From {
    /// Creates a new From handle.
    pub fn new(caller: Pid, reference: Ref) -> Self {
        Self { caller, reference }
    }
}

/// Actions to execute on state entry/exit or during transitions.
#[derive(Debug, Clone)]
pub enum StateAction<R> {
    /// No action.
    None,
    /// Reply to a pending call.
    Reply(From, R),
    /// Set a state timeout - sends Timeout event after duration.
    StateTimeout(Duration),
    /// Set a generic timeout with a name.
    GenericTimeout(String, Duration),
    /// Cancel a named timeout.
    CancelTimeout(String),
    /// Postpone the current event to be retried in the next state.
    Postpone,
}

/// Result of the `init` callback.
#[derive(Debug)]
pub enum InitResult<S, D> {
    /// Initialization succeeded with state and data.
    Ok(S, D),
    /// Initialization succeeded with a state timeout.
    OkTimeout(S, D, Duration),
    /// Initialization ignored; process will exit normally.
    Ignore,
    /// Initialization failed; process will exit with the given reason.
    Stop(ExitReason),
}

impl<S, D> InitResult<S, D> {
    /// Creates a successful init result.
    pub fn ok(state: S, data: D) -> Self {
        InitResult::Ok(state, data)
    }

    /// Creates a successful init result with a state timeout.
    pub fn ok_timeout(state: S, data: D, timeout: Duration) -> Self {
        InitResult::OkTimeout(state, data, timeout)
    }

    /// Creates a stop result.
    pub fn stop(reason: ExitReason) -> Self {
        InitResult::Stop(reason)
    }

    /// Creates an ignored init result.
    pub fn ignore() -> Self {
        InitResult::Ignore
    }
}

/// Result of the `handle_event` callback.
#[derive(Debug)]
pub enum EventResult<S, R> {
    /// Transition to a new state.
    NextState(S),
    /// Transition to a new state with a timeout.
    NextStateTimeout(S, Duration),
    /// Stay in the current state (keep_state).
    KeepState,
    /// Stay in the current state with a timeout.
    KeepStateTimeout(Duration),
    /// Transition with actions.
    NextStateActions(S, Vec<StateAction<R>>),
    /// Keep state with actions.
    KeepStateActions(Vec<StateAction<R>>),
    /// Stop the FSM.
    Stop(ExitReason),
    /// Stop with a reply.
    StopReply(ExitReason, From, R),
}

impl<S, R> EventResult<S, R> {
    /// Transition to a new state.
    pub fn next_state(state: S) -> Self {
        EventResult::NextState(state)
    }

    /// Transition to a new state with a timeout.
    pub fn next_state_timeout(state: S, timeout: Duration) -> Self {
        EventResult::NextStateTimeout(state, timeout)
    }

    /// Stay in the current state.
    pub fn keep_state() -> Self {
        EventResult::KeepState
    }

    /// Stay in the current state with a timeout.
    pub fn keep_state_timeout(timeout: Duration) -> Self {
        EventResult::KeepStateTimeout(timeout)
    }

    /// Transition to a new state with actions.
    pub fn next_state_actions(state: S, actions: Vec<StateAction<R>>) -> Self {
        EventResult::NextStateActions(state, actions)
    }

    /// Stay in current state with actions.
    pub fn keep_state_actions(actions: Vec<StateAction<R>>) -> Self {
        EventResult::KeepStateActions(actions)
    }

    /// Stop the FSM.
    pub fn stop(reason: ExitReason) -> Self {
        EventResult::Stop(reason)
    }

    /// Stop the FSM with a reply to a pending call.
    pub fn stop_reply(reason: ExitReason, from: From, reply: R) -> Self {
        EventResult::StopReply(reason, from, reply)
    }
}

/// Result of the `handle_call` callback.
#[derive(Debug)]
pub enum CallResult<S, R> {
    /// Reply and transition to a new state.
    Reply(R, S),
    /// Reply, transition, and set a state timeout.
    ReplyTimeout(R, S, Duration),
    /// Don't reply yet; transition to new state.
    NoReply(S),
    /// Don't reply yet; transition with timeout.
    NoReplyTimeout(S, Duration),
    /// Keep state and reply.
    KeepStateReply(R),
    /// Keep state without reply.
    KeepState,
    /// Stop with a reply.
    Stop(ExitReason, R, S),
    /// Stop without a reply.
    StopNoReply(ExitReason, S),
}

impl<S, R> CallResult<S, R> {
    /// Reply and transition to a new state.
    pub fn reply(reply: R, state: S) -> Self {
        CallResult::Reply(reply, state)
    }

    /// Reply, transition, and set a state timeout.
    pub fn reply_timeout(reply: R, state: S, timeout: Duration) -> Self {
        CallResult::ReplyTimeout(reply, state, timeout)
    }

    /// Don't reply yet; transition to new state.
    pub fn noreply(state: S) -> Self {
        CallResult::NoReply(state)
    }

    /// Keep state and reply.
    pub fn keep_state_reply(reply: R) -> Self {
        CallResult::KeepStateReply(reply)
    }

    /// Keep state without reply.
    pub fn keep_state() -> Self {
        CallResult::KeepState
    }

    /// Stop with a reply.
    pub fn stop(reason: ExitReason, reply: R, state: S) -> Self {
        CallResult::Stop(reason, reply, state)
    }

    /// Stop without a reply.
    pub fn stop_noreply(reason: ExitReason, state: S) -> Self {
        CallResult::StopNoReply(reason, state)
    }
}
