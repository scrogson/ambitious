//! # Ambitious - Distributed Rust Erlang Abstract Machine
//!
//! Ambitious is a native Rust implementation of Erlang/OTP primitives, providing
//! type-safe processes, message passing, supervision trees, and application
//! lifecycle management.
//!
//! # Overview
//!
//! Ambitious brings Erlang's battle-tested concurrency primitives to Rust:
//!
//! - **Processes**: Lightweight, isolated units of concurrency with mailboxes
//! - **Links**: Bidirectional failure propagation between processes
//! - **Monitors**: Unidirectional process observation
//! - **GenServer**: Generic server pattern for stateful processes
//! - **Supervisor**: Automatic process restart and fault tolerance
//! - **Application**: Top-level lifecycle management with dependencies
//!
//! # Quick Start
//!
//! ```ignore
//! #[ambitious::main]
//! async fn main() {
//!     // Spawn a process using the global runtime
//!     let pid = ambitious::spawn(|ctx| async move {
//!         println!("Hello from process {:?}", ctx.pid());
//!     });
//!
//!     // Or get the handle for more control
//!     let handle = ambitious::handle();
//!     handle.registry().send_raw(pid, b"Hello!".to_vec());
//! }
//! ```
//!
//! # GenServer Example
//!
//! ```ignore
//! use ambitious::prelude::*;
//! use ambitious::gen_server::{async_trait, GenServer, InitResult, CallResult, CastResult, InfoResult, ContinueResult, From, ContinueArg};
//! use serde::{Serialize, Deserialize};
//!
//! struct Counter;
//!
//! #[derive(Debug, Clone, Serialize, Deserialize)]
//! enum CounterCall {
//!     Get,
//!     Increment,
//! }
//!
//! #[async_trait]
//! impl GenServer for Counter {
//!     type State = i64;
//!     type InitArg = i64;
//!     type Call = CounterCall;
//!     type Cast = ();
//!     type Reply = i64;
//!
//!     async fn init(initial: i64) -> InitResult<i64> {
//!         InitResult::Ok(initial)
//!     }
//!
//!     async fn handle_call(
//!         request: CounterCall,
//!         _from: From,
//!         state: &mut i64,
//!     ) -> CallResult<i64, i64> {
//!         match request {
//!             CounterCall::Get => CallResult::Reply(*state, *state),
//!             CounterCall::Increment => {
//!                 *state += 1;
//!                 CallResult::Reply(*state, *state)
//!             }
//!         }
//!     }
//!
//!     async fn handle_cast(_msg: (), state: &mut i64) -> CastResult<i64> {
//!         CastResult::NoReply(*state)
//!     }
//!
//!     async fn handle_info(_msg: RawTerm, state: &mut i64) -> InfoResult<i64> {
//!         InfoResult::NoReply(*state)
//!     }
//!
//!     async fn handle_continue(_arg: ContinueArg, state: &mut i64) -> ContinueResult<i64> {
//!         ContinueResult::NoReply(*state)
//!     }
//! }
//! ```
//!
//! # Supervisor Example
//!
//! ```ignore
//! use ambitious::prelude::*;
//! use ambitious::supervisor::{Supervisor, ChildSpec, Strategy, SupervisorFlags};
//!
//! struct MySupervisor;
//!
//! impl Supervisor for MySupervisor {
//!     fn init(handle: &RuntimeHandle) -> (SupervisorFlags, Vec<ChildSpec>) {
//!         let flags = SupervisorFlags::new(Strategy::OneForOne)
//!             .max_restarts(3)
//!             .max_seconds(5);
//!
//!         let children = vec![
//!             // Define child specs here
//!         ];
//!
//!         (flags, children)
//!     }
//! }
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

// =============================================================================
// Core modules (merged from separate crates)
// =============================================================================

/// Interned strings for efficient comparison.
pub mod atom;

/// Core types: Pid, Ref, Term, ExitReason, SystemMessage.
pub mod core;

/// Runtime infrastructure: mailbox, context, process registry.
pub mod runtime;

/// Process primitives: spawn, link, monitor.
pub mod process;

/// GenServer pattern for stateful request/response servers.
pub mod gen_server;

/// GenFsm pattern for finite state machines.
pub mod gen_fsm;

/// Supervisor pattern for fault-tolerant process trees.
pub mod supervisor;

/// Application lifecycle management.
pub mod application;

// =============================================================================
// Additional modules
// =============================================================================

/// Distribution layer for connecting Ambitious nodes.
#[allow(dead_code)]
pub mod distribution;

/// Elixir-compatible Node API for distributed Ambitious.
pub mod node;

/// Local process registry with pub/sub support.
pub mod registry;

/// Phoenix-style Channels for real-time communication.
pub mod channel;

/// Distributed Presence tracking for real-time applications.
pub mod presence;

/// Phoenix-style PubSub for distributed publish-subscribe messaging.
pub mod pubsub;

/// Timer module for scheduling delayed and repeated operations.
pub mod timer;

/// Process-owned concurrent key-value storage (ETS-like).
pub mod store;

/// Peer node management for spawning and controlling linked nodes.
///
/// This module is only available when the `peer` feature is enabled.
#[cfg(feature = "peer")]
pub mod peer;

/// Alias for distribution module.
pub use distribution as dist;

// =============================================================================
// Re-exports for convenient top-level access
// =============================================================================

// Re-export global runtime functions from process module
pub use process::global::{
    alive, handle, init, register, spawn, spawn_link, try_handle, unregister, whereis,
};

// Re-export task-local functions for process operations without ctx
pub use runtime::{
    current_pid, recv, recv_timeout, send, send_raw, try_current_pid, try_recv, with_ctx,
    with_ctx_async,
};

// Re-export core types
pub use core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, RawTerm, Ref, Term};

// Re-export runtime and process types
pub use process::{Runtime, RuntimeHandle};
pub use runtime::Context;

// Re-export macros (from separate proc-macro crate)
pub use ambitious_macros::{GenServerImpl, ambitious_process, main, self_pid, test};

/// Prelude module for convenient imports.
///
/// Import everything commonly needed with:
/// ```ignore
/// use ambitious::prelude::*;
/// ```
pub mod prelude {
    // Core types
    pub use crate::core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, RawTerm, Ref, Term};

    // Runtime and process
    pub use crate::process::{Runtime, RuntimeHandle};
    pub use crate::runtime::Context;

    // GenServer essentials
    pub use crate::gen_server::{
        CallResult, CastResult, ContinueArg, ContinueResult, From, GenServer, InfoResult,
        InitResult, NameResolver, ServerRef,
    };

    // Supervisor essentials
    pub use crate::supervisor::{
        ChildSpec, ChildType, RestartType, ShutdownType, Strategy, Supervisor, SupervisorFlags,
    };

    // Application essentials
    pub use crate::application::{AppConfig, AppController, AppSpec, Application, StartResult};

    // Macros
    pub use ambitious_macros::{GenServerImpl, ambitious_process, main, self_pid};

    // Task-local functions for process operations without ctx
    pub use crate::runtime::{
        current_pid, recv, recv_timeout, send, send_raw, try_current_pid, try_recv, with_ctx,
        with_ctx_async,
    };

    // Node API essentials
    pub use crate::node::{ListOption, PingResult};

    // Timer essentials
    pub use crate::timer::{TimerError, TimerRef, TimerResult};

    // Store essentials
    pub use crate::store::{Access, OrderedStore, Store, StoreError, StoreId, StoreOptions};
}

#[cfg(test)]
mod tests {
    use super::prelude::*;

    #[test]
    fn test_prelude_imports() {
        // This test verifies that all prelude imports compile
        let _pid: Option<Pid> = None;
        let _ref: Option<Ref> = None;
        let _reason = ExitReason::Normal;
    }

    #[tokio::test]
    async fn test_basic_spawn() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();

        let _pid = handle.spawn(move || async move {
            executed_clone.store(true, Ordering::SeqCst);
        });

        // Give the process time to execute
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(executed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_gen_server_integration() {
        use crate::gen_server::async_trait;
        use serde::{Deserialize, Serialize};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        struct TestServer;

        #[derive(Debug, Clone, Serialize, Deserialize)]
        enum TestCall {
            Ping,
        }

        #[async_trait]
        impl GenServer for TestServer {
            type State = ();
            type InitArg = ();
            type Call = TestCall;
            type Cast = ();
            type Reply = String;

            async fn init(_: ()) -> InitResult<()> {
                InitResult::Ok(())
            }

            async fn handle_call(
                request: TestCall,
                _from: From,
                state: &mut (),
            ) -> CallResult<(), String> {
                match request {
                    TestCall::Ping => {
                        let _ = state;
                        CallResult::Reply("pong".to_string(), ())
                    }
                }
            }

            async fn handle_cast(_: (), state: &mut ()) -> CastResult<()> {
                let _ = state;
                CastResult::NoReply(())
            }

            async fn handle_info(_: RawTerm, state: &mut ()) -> InfoResult<()> {
                let _ = state;
                InfoResult::NoReply(())
            }

            async fn handle_continue(_: ContinueArg, state: &mut ()) -> ContinueResult<()> {
                let _ = state;
                ContinueResult::NoReply(())
            }
        }

        crate::init();
        let handle = crate::handle();

        // Start the server
        let server_pid = crate::gen_server::start::<TestServer>(()).await.unwrap();

        // We need to call from within a process since `call` requires task-local context
        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();

        handle.spawn(move || async move {
            let reply: String = crate::gen_server::call::<TestServer>(
                server_pid,
                TestCall::Ping,
                Duration::from_secs(5),
            )
            .await
            .unwrap();

            if reply == "pong" {
                test_passed_clone.store(true, Ordering::SeqCst);
            }
        });

        // Give the test time to complete
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            test_passed.load(Ordering::SeqCst),
            "GenServer call should return 'pong'"
        );
    }

    #[tokio::test]
    async fn test_application_integration() {
        struct TestApp;

        impl Application for TestApp {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                Ok(StartResult::None)
            }

            fn spec() -> AppSpec {
                AppSpec::new("test_app")
            }
        }

        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        controller.register::<TestApp>();
        controller.start("test_app").await.unwrap();
        assert!(controller.is_running("test_app"));

        controller.stop("test_app").await.unwrap();
        assert!(!controller.is_running("test_app"));
    }
}
