//! # Starlang - Distributed Rust Erlang Abstract Machine
//!
//! Starlang is a native Rust implementation of Erlang/OTP primitives, providing
//! type-safe processes, message passing, supervision trees, and application
//! lifecycle management.
//!
//! # Overview
//!
//! Starlang brings Erlang's battle-tested concurrency primitives to Rust:
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
//! #[starlang::main]
//! async fn main() {
//!     // Spawn a process using the global runtime
//!     let pid = starlang::spawn(|ctx| async move {
//!         println!("Hello from process {:?}", ctx.pid());
//!     });
//!
//!     // Or get the handle for more control
//!     let handle = starlang::handle();
//!     handle.registry().send_raw(pid, b"Hello!".to_vec());
//! }
//! ```
//!
//! # GenServer Example
//!
//! ```ignore
//! use starlang::prelude::*;
//! use starlang::gen_server::{async_trait, GenServer, InitResult, CallResult, CastResult, InfoResult, ContinueResult, From, ContinueArg};
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
//!     async fn init(_ctx: &mut Context, initial: i64) -> InitResult<i64> {
//!         InitResult::Ok(initial)
//!     }
//!
//!     async fn handle_call(
//!         _ctx: &mut Context,
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
//!     async fn handle_cast(_ctx: &mut Context, _msg: (), state: &mut i64) -> CastResult<i64> {
//!         CastResult::NoReply(*state)
//!     }
//!
//!     async fn handle_info(_ctx: &mut Context, _msg: Vec<u8>, state: &mut i64) -> InfoResult<i64> {
//!         InfoResult::NoReply(*state)
//!     }
//!
//!     async fn handle_continue(_ctx: &mut Context, _arg: ContinueArg, state: &mut i64) -> ContinueResult<i64> {
//!         ContinueResult::NoReply(*state)
//!     }
//! }
//! ```
//!
//! # Supervisor Example
//!
//! ```ignore
//! use starlang::prelude::*;
//! use starlang::supervisor::{Supervisor, ChildSpec, Strategy, SupervisorFlags};
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

// Allow dead_code in distribution module until fully integrated
#[allow(dead_code)]
pub mod distribution;

/// Elixir-compatible Node API for distributed Starlang.
///
/// See [`node`] module for details.
pub mod node;

/// Local process registry with pub/sub support.
///
/// See [`registry`] module for details.
pub mod registry;

/// Phoenix-style Channels for real-time communication.
///
/// See [`channel`] module for details.
pub mod channel;

/// Distributed Presence tracking for real-time applications.
///
/// See [`presence`] module for details.
pub mod presence;

/// Distribution module for connecting Starlang nodes.
///
/// See [`distribution`] module for details.
pub use distribution as dist;

// Re-export global runtime functions from starlang_process
pub use starlang_process::global::{
    alive, handle, init, register, spawn, spawn_link, try_handle, unregister, whereis,
};

// Re-export task-local functions for process operations without ctx
pub use starlang_runtime::{
    current_pid, recv, recv_timeout, send, send_raw, try_current_pid, try_recv, with_ctx,
    with_ctx_async,
};

// Re-export core types
pub use starlang_core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, Ref, Term};

// Re-export runtime and process types
pub use starlang_process::{Runtime, RuntimeHandle};
pub use starlang_runtime::Context;

// Re-export GenServer
pub use starlang_gen_server as gen_server;

// Re-export Supervisor
pub use starlang_supervisor as supervisor;

// Re-export Application
pub use starlang_application as application;

// Re-export macros
pub use starlang_macros::{starlang_process, main, self_pid, GenServerImpl};

/// Prelude module for convenient imports.
///
/// Import everything commonly needed with:
/// ```ignore
/// use starlang::prelude::*;
/// ```
pub mod prelude {
    // Core types
    pub use starlang_core::{ExitReason, NodeId, NodeInfo, NodeName, Pid, Ref, Term};

    // Runtime and process
    pub use starlang_process::{Runtime, RuntimeHandle};
    pub use starlang_runtime::Context;

    // GenServer essentials
    pub use starlang_gen_server::{
        CallResult, CastResult, ContinueArg, ContinueResult, From, GenServer, InfoResult,
        InitResult, NameResolver, ServerRef,
    };

    // Supervisor essentials
    pub use starlang_supervisor::{
        ChildSpec, ChildType, RestartType, ShutdownType, Strategy, Supervisor, SupervisorFlags,
    };

    // Application essentials
    pub use starlang_application::{AppConfig, AppController, AppSpec, Application, StartResult};

    // Macros
    pub use starlang_macros::{starlang_process, main, self_pid, GenServerImpl};

    // Task-local functions for process operations without ctx
    pub use starlang_runtime::{
        current_pid, recv, recv_timeout, send, send_raw, try_current_pid, try_recv, with_ctx,
        with_ctx_async,
    };

    // Node API essentials
    pub use crate::node::{ListOption, PingResult};
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
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

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
        use starlang_gen_server::async_trait;
        use serde::{Deserialize, Serialize};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
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
                    TestCall::Ping => CallResult::Reply("pong".to_string(), *state),
                }
            }

            async fn handle_cast(_: (), state: &mut ()) -> CastResult<()> {
                CastResult::NoReply(*state)
            }

            async fn handle_info(_: Vec<u8>, state: &mut ()) -> InfoResult<()> {
                InfoResult::NoReply(*state)
            }

            async fn handle_continue(_: ContinueArg, state: &mut ()) -> ContinueResult<()> {
                ContinueResult::NoReply(*state)
            }
        }

        crate::init();
        let handle = crate::handle();

        // Start the server
        let server_pid = starlang_gen_server::start::<TestServer>(()).await.unwrap();

        // We need to call from within a process since `call` requires task-local context
        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();

        handle.spawn(move || async move {
            let reply: String = starlang_gen_server::call::<TestServer>(
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

        assert!(test_passed.load(Ordering::SeqCst), "GenServer call should return 'pong'");
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
