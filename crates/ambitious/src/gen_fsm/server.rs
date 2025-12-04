//! GenFsm trait and server implementation.

use super::error::{CallError, SendEventError, StartError};
use super::protocol::FsmMessage;
use super::types::{CallResult, EventResult, From, InitResult, StateAction};
use crate::core::{ExitReason, Pid, Ref, Term};
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Generic Finite State Machine trait.
///
/// Implement this trait to create a process-based state machine.
///
/// # Type Parameters
///
/// - `State`: The enum representing possible states
/// - `Event`: The enum representing events that trigger transitions
/// - `Data`: Mutable data carried across state transitions
/// - `InitArg`: Argument passed to `init`
/// - `Call`: Request type for synchronous calls
/// - `Reply`: Response type for synchronous calls
#[async_trait]
pub trait GenFsm: Sized + Send + 'static {
    /// The state enum type.
    type State: Clone + Send + Sync + 'static;
    /// The event type that triggers state transitions.
    type Event: Term;
    /// Mutable data carried across states.
    type Data: Send + 'static;
    /// Argument passed to init.
    type InitArg: Send + 'static;
    /// Request type for synchronous calls.
    type Call: Term;
    /// Reply type for synchronous calls.
    type Reply: Term;

    /// Initialize the FSM with initial state and data.
    async fn init(arg: Self::InitArg) -> InitResult<Self::State, Self::Data>;

    /// Handle an event in the current state.
    ///
    /// This is the core state transition logic. Pattern match on `(state, event)`
    /// to implement your state machine transitions.
    async fn handle_event(
        state: &Self::State,
        event: Self::Event,
        data: &mut Self::Data,
    ) -> EventResult<Self::State, Self::Reply>;

    /// Handle a synchronous call.
    ///
    /// Override this to support request/response operations on the FSM.
    /// The default implementation keeps state without replying.
    async fn handle_call(
        _state: &Self::State,
        _request: Self::Call,
        _from: From,
        _data: &mut Self::Data,
    ) -> CallResult<Self::State, Self::Reply> {
        CallResult::keep_state()
    }

    /// Called when entering a new state.
    ///
    /// Override for side effects on state entry.
    async fn enter_state(_state: &Self::State, _data: &mut Self::Data) {}

    /// Called when exiting a state.
    ///
    /// Override for cleanup on state exit.
    async fn exit_state(_state: &Self::State, _data: &mut Self::Data) {}

    /// Called when the FSM is terminating.
    async fn terminate(_reason: ExitReason, _state: &Self::State, _data: &mut Self::Data) {}
}

/// Start a GenFsm process.
pub async fn start<F: GenFsm>(arg: F::InitArg) -> Result<Pid, StartError> {
    let (tx, rx) = oneshot::channel();

    let handle = crate::process::global::handle();
    let pid = handle.spawn(move || async move {
        run_fsm::<F>(arg, tx).await;
    });

    // Wait for init result
    match rx.await {
        Ok(Ok(())) => Ok(pid),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::SpawnFailed),
    }
}

/// Start a linked GenFsm process.
pub async fn start_link<F: GenFsm>(arg: F::InitArg) -> Result<Pid, StartError> {
    let (tx, rx) = oneshot::channel();

    let handle = crate::process::global::handle();
    let parent_pid = crate::runtime::current_pid();
    let pid = handle.spawn_link(parent_pid, move || async move {
        run_fsm::<F>(arg, tx).await;
    });

    match rx.await {
        Ok(Ok(())) => Ok(pid),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::SpawnFailed),
    }
}

/// Send an event to a GenFsm process (async, fire-and-forget).
pub fn send_event<F: GenFsm>(pid: Pid, event: F::Event) -> Result<(), SendEventError> {
    let msg = FsmMessage::Event(event.encode());
    let encoded = msg.encode();

    let handle = crate::process::global::handle();
    handle
        .registry()
        .send_raw(pid, encoded)
        .map_err(|_| SendEventError::ProcessNotFound(pid))
}

/// Make a synchronous call to a GenFsm process.
pub async fn call<F: GenFsm>(
    pid: Pid,
    request: F::Call,
    timeout: Duration,
) -> Result<F::Reply, CallError> {
    let reference = Ref::new();
    let caller = crate::runtime::current_pid();

    let msg = FsmMessage::Call {
        from: caller,
        reference,
        request: request.encode(),
    };

    let handle = crate::process::global::handle();
    handle
        .registry()
        .send_raw(pid, msg.encode())
        .map_err(|_| CallError::ProcessNotFound(pid))?;

    // Wait for reply
    match crate::runtime::recv_timeout(timeout).await {
        Ok(Some(data)) => {
            // Decode the FsmMessage::Reply
            match FsmMessage::decode(&data) {
                Ok(FsmMessage::Reply { reply, .. }) => {
                    F::Reply::decode(&reply).map_err(|e| CallError::DecodeError(format!("{:?}", e)))
                }
                _ => Err(CallError::DecodeError("unexpected message type".into())),
            }
        }
        Ok(None) => Err(CallError::Stopped(ExitReason::Normal)),
        Err(()) => Err(CallError::Timeout),
    }
}

// =============================================================================
// Internal Timer Management
// =============================================================================

/// Active timer handle with cancellation.
struct ActiveTimer {
    cancel_tx: mpsc::Sender<()>,
}

/// Manages timeouts for the FSM.
struct TimeoutManager {
    /// The current state timeout (only one at a time).
    state_timeout: Option<ActiveTimer>,
    /// Named generic timeouts.
    generic_timeouts: HashMap<String, ActiveTimer>,
    /// Channel to receive timeout notifications.
    timeout_rx: mpsc::Receiver<TimeoutEvent>,
    /// Channel sender for spawned timeout tasks.
    timeout_tx: mpsc::Sender<TimeoutEvent>,
}

/// Timeout event sent back to the FSM loop.
#[derive(Debug)]
enum TimeoutEvent {
    State,
    Generic(String),
}

impl TimeoutManager {
    fn new() -> Self {
        let (timeout_tx, timeout_rx) = mpsc::channel(16);
        Self {
            state_timeout: None,
            generic_timeouts: HashMap::new(),
            timeout_rx,
            timeout_tx,
        }
    }

    /// Schedule a state timeout. Cancels any existing state timeout.
    fn set_state_timeout(&mut self, duration: Duration) {
        // Cancel existing state timeout
        self.cancel_state_timeout();

        let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
        let tx = self.timeout_tx.clone();

        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel_rx.recv() => {
                    // Cancelled
                }
                _ = tokio::time::sleep(duration) => {
                    let _ = tx.send(TimeoutEvent::State).await;
                }
            }
        });

        self.state_timeout = Some(ActiveTimer { cancel_tx });
    }

    /// Cancel the state timeout.
    fn cancel_state_timeout(&mut self) {
        if let Some(timer) = self.state_timeout.take() {
            let _ = timer.cancel_tx.try_send(());
        }
    }

    /// Schedule a named generic timeout.
    fn set_generic_timeout(&mut self, name: String, duration: Duration) {
        // Cancel existing timeout with this name
        self.cancel_generic_timeout(&name);

        let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
        let tx = self.timeout_tx.clone();
        let name_clone = name.clone();

        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel_rx.recv() => {
                    // Cancelled
                }
                _ = tokio::time::sleep(duration) => {
                    let _ = tx.send(TimeoutEvent::Generic(name_clone)).await;
                }
            }
        });

        self.generic_timeouts
            .insert(name, ActiveTimer { cancel_tx });
    }

    /// Cancel a named generic timeout.
    fn cancel_generic_timeout(&mut self, name: &str) {
        if let Some(timer) = self.generic_timeouts.remove(name) {
            let _ = timer.cancel_tx.try_send(());
        }
    }

    /// Cancel all timeouts (called on state change).
    fn cancel_all(&mut self) {
        self.cancel_state_timeout();
        for (_, timer) in self.generic_timeouts.drain() {
            let _ = timer.cancel_tx.try_send(());
        }
    }

    /// Try to receive a timeout event without blocking.
    fn try_recv(&mut self) -> Option<TimeoutEvent> {
        self.timeout_rx.try_recv().ok()
    }
}

// =============================================================================
// FSM Loop
// =============================================================================

/// Internal: Run the FSM process loop.
async fn run_fsm<F: GenFsm>(arg: F::InitArg, init_tx: oneshot::Sender<Result<(), StartError>>) {
    let mut timeout_mgr = TimeoutManager::new();
    let mut postponed_events: VecDeque<Vec<u8>> = VecDeque::new();

    // Initialize
    let (mut state, mut data) = match F::init(arg).await {
        InitResult::Ok(s, d) => {
            let _ = init_tx.send(Ok(()));
            (s, d)
        }
        InitResult::OkTimeout(s, d, timeout) => {
            let _ = init_tx.send(Ok(()));
            timeout_mgr.set_state_timeout(timeout);
            (s, d)
        }
        InitResult::Ignore => {
            let _ = init_tx.send(Err(StartError::Ignore));
            return;
        }
        InitResult::Stop(reason) => {
            let _ = init_tx.send(Err(StartError::Stop(reason)));
            return;
        }
    };

    // Call enter_state for initial state
    F::enter_state(&state, &mut data).await;

    // Main loop
    loop {
        // First, process any postponed events
        let msg_data = if let Some(postponed) = postponed_events.pop_front() {
            postponed
        } else {
            // Check for timeout events
            if let Some(timeout_event) = timeout_mgr.try_recv() {
                match timeout_event {
                    TimeoutEvent::State => FsmMessage::StateTimeout.encode(),
                    TimeoutEvent::Generic(name) => FsmMessage::GenericTimeout(name).encode(),
                }
            } else {
                // Wait for next message or timeout
                tokio::select! {
                    biased;
                    Some(timeout_event) = timeout_mgr.timeout_rx.recv() => {
                        match timeout_event {
                            TimeoutEvent::State => FsmMessage::StateTimeout.encode(),
                            TimeoutEvent::Generic(name) => FsmMessage::GenericTimeout(name).encode(),
                        }
                    }
                    msg = crate::runtime::recv() => {
                        match msg {
                            Some(data) => data,
                            None => break, // Mailbox closed
                        }
                    }
                }
            }
        };

        let msg = match FsmMessage::decode(&msg_data) {
            Ok(m) => m,
            Err(_) => continue, // Skip malformed messages
        };

        // Track if we should postpone this event
        let mut should_postpone = false;
        let current_event_data = msg_data.clone();

        match msg {
            FsmMessage::Event(event_data) => {
                let event = match F::Event::decode(&event_data) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                let result = F::handle_event(&state, event, &mut data).await;
                handle_event_result::<F>(
                    result,
                    &mut state,
                    &mut data,
                    &mut timeout_mgr,
                    &mut should_postpone,
                )
                .await;
            }
            FsmMessage::Call {
                from,
                reference,
                request,
            } => {
                let req = match F::Call::decode(&request) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let from_handle = From::new(from, reference);
                let result = F::handle_call(&state, req, from_handle, &mut data).await;

                if handle_call_result::<F>(
                    result,
                    from_handle,
                    &mut state,
                    &mut data,
                    &mut timeout_mgr,
                )
                .await
                {
                    // Stop requested
                    break;
                }
            }
            FsmMessage::Reply { .. } => {
                // Replies are handled by call() directly, shouldn't reach here
            }
            FsmMessage::StateTimeout => {
                // Generate a timeout event - create internal timeout message
                // FSMs should define a Timeout variant in their Event enum
                // For now, we'll send it as an encoded "state_timeout" message
                let timeout_msg = FsmMessage::Event(b"state_timeout".to_vec());
                let _ =
                    crate::runtime::send_raw(crate::runtime::current_pid(), timeout_msg.encode());
            }
            FsmMessage::GenericTimeout(name) => {
                // Generic timeout - encode the name as the event
                let timeout_msg = FsmMessage::Event(name.as_bytes().to_vec());
                let _ =
                    crate::runtime::send_raw(crate::runtime::current_pid(), timeout_msg.encode());
            }
        }

        // If postpone was requested, add to queue
        if should_postpone {
            postponed_events.push_back(current_event_data);
        }
    }

    F::terminate(ExitReason::Normal, &state, &mut data).await;
}

/// Handle EventResult and update state/timeouts accordingly.
async fn handle_event_result<F: GenFsm>(
    result: EventResult<F::State, F::Reply>,
    state: &mut F::State,
    data: &mut F::Data,
    timeout_mgr: &mut TimeoutManager,
    should_postpone: &mut bool,
) {
    match result {
        EventResult::NextState(new_state) => {
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
        }
        EventResult::NextStateTimeout(new_state, timeout) => {
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
            timeout_mgr.set_state_timeout(timeout);
        }
        EventResult::KeepState => {
            // Stay in current state, cancel state timeout
            timeout_mgr.cancel_state_timeout();
        }
        EventResult::KeepStateTimeout(timeout) => {
            // Stay in current state, set new timeout
            timeout_mgr.set_state_timeout(timeout);
        }
        EventResult::NextStateActions(new_state, actions) => {
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
            process_actions::<F>(&actions, timeout_mgr, should_postpone);
        }
        EventResult::KeepStateActions(actions) => {
            timeout_mgr.cancel_state_timeout();
            process_actions::<F>(&actions, timeout_mgr, should_postpone);
        }
        EventResult::Stop(reason) => {
            F::terminate(reason, state, data).await;
            // Caller should break the loop
        }
        EventResult::StopReply(reason, from, reply) => {
            send_reply(from, &reply);
            F::terminate(reason, state, data).await;
            // Caller should break the loop
        }
    }
}

/// Handle CallResult and update state/timeouts accordingly.
/// Returns true if the FSM should stop.
async fn handle_call_result<F: GenFsm>(
    result: CallResult<F::State, F::Reply>,
    from: From,
    state: &mut F::State,
    data: &mut F::Data,
    timeout_mgr: &mut TimeoutManager,
) -> bool {
    match result {
        CallResult::Reply(reply, new_state) => {
            send_reply(from, &reply);
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
            false
        }
        CallResult::ReplyTimeout(reply, new_state, timeout) => {
            send_reply(from, &reply);
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
            timeout_mgr.set_state_timeout(timeout);
            false
        }
        CallResult::NoReply(new_state) => {
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
            false
        }
        CallResult::NoReplyTimeout(new_state, timeout) => {
            transition_state::<F>(state, new_state, data, timeout_mgr).await;
            timeout_mgr.set_state_timeout(timeout);
            false
        }
        CallResult::KeepStateReply(reply) => {
            send_reply(from, &reply);
            false
        }
        CallResult::KeepState => {
            // Stay in current state, no reply
            false
        }
        CallResult::Stop(reason, reply, new_state) => {
            send_reply(from, &reply);
            *state = new_state;
            F::terminate(reason, state, data).await;
            true
        }
        CallResult::StopNoReply(reason, new_state) => {
            *state = new_state;
            F::terminate(reason, state, data).await;
            true
        }
    }
}

/// Transition to a new state, calling exit/enter hooks.
async fn transition_state<F: GenFsm>(
    state: &mut F::State,
    new_state: F::State,
    data: &mut F::Data,
    timeout_mgr: &mut TimeoutManager,
) {
    // Cancel all timeouts on state change
    timeout_mgr.cancel_all();

    // Call exit hook for old state
    F::exit_state(state, data).await;

    // Update state
    *state = new_state;

    // Call enter hook for new state
    F::enter_state(state, data).await;
}

/// Process state actions.
fn process_actions<F: GenFsm>(
    actions: &[StateAction<F::Reply>],
    timeout_mgr: &mut TimeoutManager,
    should_postpone: &mut bool,
) {
    for action in actions {
        match action {
            StateAction::None => {}
            StateAction::Reply(from, reply) => {
                send_reply(*from, reply);
            }
            StateAction::StateTimeout(duration) => {
                timeout_mgr.set_state_timeout(*duration);
            }
            StateAction::GenericTimeout(name, duration) => {
                timeout_mgr.set_generic_timeout(name.clone(), *duration);
            }
            StateAction::CancelTimeout(name) => {
                timeout_mgr.cancel_generic_timeout(name);
            }
            StateAction::Postpone => {
                *should_postpone = true;
            }
        }
    }
}

/// Send a reply to a caller.
fn send_reply<R: Term>(from: From, reply: &R) {
    let msg = FsmMessage::Reply {
        reference: from.reference,
        reply: reply.encode(),
    };

    let handle = crate::process::global::handle();
    let _ = handle.registry().send_raw(from.caller, msg.encode());
}
