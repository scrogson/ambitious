//! # dream-gen-server
//!
//! GenServer pattern implementation for DREAM.
//!
//! This crate provides the `GenServer` trait and related types for implementing
//! request/response servers, mirroring Elixir's GenServer behavior.
//!
//! # Overview
//!
//! A GenServer is a process that:
//! - Maintains internal state
//! - Handles synchronous calls (request/response)
//! - Handles asynchronous casts (fire-and-forget)
//! - Handles arbitrary info messages
//!
//! # Example
//!
//! ```ignore
//! use dream_gen_server::{GenServer, InitResult, CallResult, CastResult, From};
//! use serde::{Serialize, Deserialize};
//!
//! // Define a simple counter server
//! struct Counter;
//!
//! #[derive(Serialize, Deserialize)]
//! enum CounterCall {
//!     Get,
//!     Increment,
//! }
//!
//! #[derive(Serialize, Deserialize)]
//! enum CounterCast {
//!     Reset,
//! }
//!
//! impl GenServer for Counter {
//!     type State = i64;
//!     type InitArg = i64;
//!     type Call = CounterCall;
//!     type Cast = CounterCast;
//!     type Reply = i64;
//!
//!     fn init(initial: i64) -> InitResult<i64> {
//!         InitResult::ok(initial)
//!     }
//!
//!     fn handle_call(
//!         request: CounterCall,
//!         _from: From,
//!         state: &mut i64,
//!     ) -> CallResult<i64, i64> {
//!         match request {
//!             CounterCall::Get => CallResult::reply(*state, *state),
//!             CounterCall::Increment => {
//!                 *state += 1;
//!                 CallResult::reply(*state, *state)
//!             }
//!         }
//!     }
//!
//!     fn handle_cast(msg: CounterCast, state: &mut i64) -> CastResult<i64> {
//!         match msg {
//!             CounterCast::Reset => CastResult::noreply(0),
//!         }
//!     }
//! }
//! ```
//!
//! # Client API
//!
//! ```ignore
//! use dream_gen_server::{start, call, cast, stop};
//! use std::time::Duration;
//!
//! // Start the server
//! let pid = start::<Counter>(&handle, 0).await?;
//!
//! // Make a synchronous call
//! let value = call::<Counter>(&handle, &mut ctx, pid, CounterCall::Get, Duration::from_secs(5)).await?;
//!
//! // Send an async cast
//! cast::<Counter>(&handle, pid, CounterCast::Reset)?;
//!
//! // Stop the server
//! stop(&handle, &mut ctx, pid, ExitReason::Normal, Duration::from_secs(5)).await?;
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

mod error;
mod protocol;
mod server;
mod types;

pub use async_trait::async_trait;
pub use error::{CallError, StartError, StopError};
pub use server::{call, cast, reply, start, start_link, stop, GenServer};
pub use types::{
    CallResult, CastResult, ContinueArg, ContinueResult, From, InfoResult, InitResult,
    NameResolver, ServerRef,
};

// Re-export commonly used types
pub use dream_core::{ExitReason, Pid, Ref, Term};

/// Prelude module for convenient imports.
///
/// Import everything needed to implement a GenServer with:
/// ```ignore
/// use dream::gen_server::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        async_trait, call, cast, start, start_link, stop, CallError, CallResult, CastResult,
        ContinueArg, ContinueResult, ExitReason, From, GenServer, InfoResult, InitResult, Pid,
        StartError, StopError,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::sleep;

    // A simple counter GenServer for testing
    struct Counter;

    #[derive(Debug, Serialize, Deserialize)]
    enum CounterCall {
        Get,
        Increment,
        Add(i64),
    }

    #[derive(Debug, Serialize, Deserialize)]
    enum CounterCast {
        Reset,
        Set(i64),
    }

    #[async_trait]
    impl GenServer for Counter {
        type State = i64;
        type InitArg = i64;
        type Call = CounterCall;
        type Cast = CounterCast;
        type Reply = i64;

        async fn init(initial: i64) -> InitResult<i64> {
            InitResult::ok(initial)
        }

        async fn handle_call(
            request: CounterCall,
            _from: From,
            state: &mut i64,
        ) -> CallResult<i64, i64> {
            match request {
                CounterCall::Get => CallResult::reply(*state, *state),
                CounterCall::Increment => {
                    *state += 1;
                    CallResult::reply(*state, *state)
                }
                CounterCall::Add(n) => {
                    *state += n;
                    CallResult::reply(*state, *state)
                }
            }
        }

        async fn handle_cast(msg: CounterCast, _state: &mut i64) -> CastResult<i64> {
            match msg {
                CounterCast::Reset => CastResult::noreply(0),
                CounterCast::Set(n) => CastResult::noreply(n),
            }
        }

        async fn handle_info(_msg: Vec<u8>, state: &mut i64) -> InfoResult<i64> {
            CastResult::noreply(*state)
        }

        async fn handle_continue(_arg: ContinueArg, state: &mut i64) -> ContinueResult<i64> {
            CastResult::noreply(*state)
        }
    }

    #[tokio::test]
    async fn test_start_gen_server() {
        dream_process::global::init();
        let handle = dream_process::global::handle();

        let pid = start::<Counter>(42).await.unwrap();
        assert!(handle.alive(pid));

        // Give it time to run
        sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_gen_server_call() {
        dream_process::global::init();
        let handle = dream_process::global::handle();

        let server_pid = start::<Counter>(10).await.unwrap();

        // Create a client process to make the call
        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        let _client_pid = handle.spawn(move || async move {
            // Make a call to get the current value
            match call::<Counter>(server_pid, CounterCall::Get, Duration::from_secs(5)).await {
                Ok(value) => {
                    result_clone.store(value, Ordering::SeqCst);
                }
                Err(_) => {
                    result_clone.store(-999, Ordering::SeqCst);
                }
            }
        });

        // Wait for the call to complete
        sleep(Duration::from_millis(100)).await;

        assert_eq!(result.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn test_gen_server_cast() {
        dream_process::global::init();
        let handle = dream_process::global::handle();

        let server_pid = start::<Counter>(10).await.unwrap();

        // Send a cast to reset the counter
        cast::<Counter>(server_pid, CounterCast::Reset).unwrap();

        // Give it time to process
        sleep(Duration::from_millis(50)).await;

        // Verify by making a call
        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        let _client_pid = handle.spawn(move || async move {
            match call::<Counter>(server_pid, CounterCall::Get, Duration::from_secs(5)).await {
                Ok(value) => {
                    result_clone.store(value, Ordering::SeqCst);
                }
                Err(_) => {}
            }
        });

        sleep(Duration::from_millis(100)).await;

        assert_eq!(result.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_init_stop() {
        struct StoppingServer;

        #[async_trait]
        impl GenServer for StoppingServer {
            type State = ();
            type InitArg = bool;
            type Call = ();
            type Cast = ();
            type Reply = ();

            async fn init(should_stop: bool) -> InitResult<()> {
                if should_stop {
                    InitResult::stop(ExitReason::error("init failed"))
                } else {
                    InitResult::ok(())
                }
            }

            async fn handle_call(_: (), _: From, _: &mut ()) -> CallResult<(), ()> {
                CallResult::reply((), ())
            }

            async fn handle_cast(_: (), _: &mut ()) -> CastResult<()> {
                CastResult::noreply(())
            }

            async fn handle_info(_: Vec<u8>, _: &mut ()) -> InfoResult<()> {
                CastResult::noreply(())
            }

            async fn handle_continue(_: ContinueArg, _: &mut ()) -> ContinueResult<()> {
                CastResult::noreply(())
            }
        }

        dream_process::global::init();

        // Should start successfully
        let pid = start::<StoppingServer>(false).await;
        assert!(pid.is_ok());

        // Should fail to start
        let result = start::<StoppingServer>(true).await;
        assert!(matches!(result, Err(StartError::Stop(_))));
    }

    #[tokio::test]
    async fn test_init_ignore() {
        struct IgnoringServer;

        #[async_trait]
        impl GenServer for IgnoringServer {
            type State = ();
            type InitArg = ();
            type Call = ();
            type Cast = ();
            type Reply = ();

            async fn init(_: ()) -> InitResult<()> {
                InitResult::ignore()
            }

            async fn handle_call(_: (), _: From, _: &mut ()) -> CallResult<(), ()> {
                CallResult::reply((), ())
            }

            async fn handle_cast(_: (), _: &mut ()) -> CastResult<()> {
                CastResult::noreply(())
            }

            async fn handle_info(_: Vec<u8>, _: &mut ()) -> InfoResult<()> {
                CastResult::noreply(())
            }

            async fn handle_continue(_: ContinueArg, _: &mut ()) -> ContinueResult<()> {
                CastResult::noreply(())
            }
        }

        dream_process::global::init();

        let result = start::<IgnoringServer>(()).await;
        assert!(matches!(result, Err(StartError::Ignore)));
    }

    #[tokio::test]
    async fn test_terminate_callback() {
        static TERMINATED: AtomicI64 = AtomicI64::new(0);

        struct TerminatingServer;

        #[async_trait]
        impl GenServer for TerminatingServer {
            type State = i64;
            type InitArg = i64;
            type Call = bool; // true = stop
            type Cast = ();
            type Reply = ();

            async fn init(v: i64) -> InitResult<i64> {
                InitResult::ok(v)
            }

            async fn handle_call(stop: bool, _: From, state: &mut i64) -> CallResult<i64, ()> {
                if stop {
                    CallResult::stop(ExitReason::Normal, (), *state)
                } else {
                    CallResult::reply((), *state)
                }
            }

            async fn handle_cast(_: (), state: &mut i64) -> CastResult<i64> {
                CastResult::noreply(*state)
            }

            async fn handle_info(_: Vec<u8>, state: &mut i64) -> InfoResult<i64> {
                CastResult::noreply(*state)
            }

            async fn handle_continue(_: ContinueArg, state: &mut i64) -> ContinueResult<i64> {
                CastResult::noreply(*state)
            }

            async fn terminate(_reason: ExitReason, state: &mut i64) {
                TERMINATED.store(*state, Ordering::SeqCst);
            }
        }

        dream_process::global::init();
        let handle = dream_process::global::handle();

        let server_pid = start::<TerminatingServer>(42).await.unwrap();

        // Send a call that stops the server
        let _client_pid = handle.spawn(move || async move {
            let _ = call::<TerminatingServer>(server_pid, true, Duration::from_secs(5)).await;
        });

        sleep(Duration::from_millis(100)).await;

        assert_eq!(TERMINATED.load(Ordering::SeqCst), 42);
    }
}
