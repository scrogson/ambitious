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
//! use ambitious::gen_server::*;
//! use ambitious::{call, cast, info, Message};
//!
//! // The struct IS the process state
//! struct Counter {
//!     count: i64,
//! }
//!
//! // Messages
//! #[derive(Message)]
//! struct Get;
//!
//! #[derive(Message)]
//! struct Increment;
//!
//! impl GenServer for Counter {
//!     type Args = i64;
//!
//!     async fn init(initial: i64) -> Init<Self> {
//!         Init::Ok(Counter { count: initial })
//!     }
//! }
//!
//! #[call]
//! impl HandleCall<Get> for Counter {
//!     type Reply = i64;
//!     type Output = Reply<i64>;
//!
//!     async fn handle_call(&mut self, _msg: Get, _from: From) -> Reply<i64> {
//!         Reply::Ok(self.count)
//!     }
//! }
//!
//! #[cast]
//! impl HandleCast<Increment> for Counter {
//!     type Output = Status;
//!
//!     async fn handle_cast(&mut self, _msg: Increment) -> Status {
//!         self.count += 1;
//!         Status::Ok
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

/// Message trait for typed, self-describing messages.
pub mod message;

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
pub use ambitious_macros::{
    GenServerImpl, Message, ambitious_process, call, cast, channel, handle_in, info, main,
    self_pid, test,
};

// Re-export linkme for use by macros
pub use linkme;

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
        From, GenServer, HandleCall, HandleCast, HandleContinue, HandleInfo, HasHandlers, Init,
        Reply, Status,
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
        use crate::core::DecodeError;
        use crate::gen_server::{
            From, GenServer, HandleCall, HasHandlers, Init, RawReply, Reply, Status, async_trait,
            call, start,
        };
        use crate::message::{Message, encode_with_tag};
        use serde::{Deserialize, Serialize};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        struct TestServer;

        // Message type with manual Message impl
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct Ping;

        impl Message for Ping {
            fn tag() -> &'static str {
                "Ping"
            }
            fn encode_local(&self) -> Vec<u8> {
                encode_with_tag(Self::tag(), &[])
            }
            fn decode_local(_bytes: &[u8]) -> Result<Self, DecodeError> {
                Ok(Ping)
            }
            fn encode_remote(&self) -> Vec<u8> {
                self.encode_local()
            }
            fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
                Self::decode_local(bytes)
            }
        }

        // Register handlers
        crate::register_handlers!(TestServer { calls: [Ping] });

        #[async_trait]
        impl GenServer for TestServer {
            type Args = ();

            async fn init(_: ()) -> Init<Self> {
                Init::Ok(TestServer)
            }

            async fn handle_call_raw(&mut self, payload: Vec<u8>, from: From) -> RawReply {
                <Self as HasHandlers>::handle_call_dispatch(self, payload, from).await
            }

            async fn handle_cast_raw(&mut self, payload: Vec<u8>) -> Status {
                <Self as HasHandlers>::handle_cast_dispatch(self, payload).await
            }

            async fn handle_info_raw(&mut self, payload: Vec<u8>) -> Status {
                <Self as HasHandlers>::handle_info_dispatch(self, payload).await
            }
        }

        #[async_trait]
        impl HandleCall<Ping> for TestServer {
            type Reply = String;
            type Output = Reply<String>;

            async fn handle_call(&mut self, _msg: Ping, _from: From) -> Reply<String> {
                Reply::Ok("pong".to_string())
            }
        }

        crate::init();
        let handle = crate::handle();

        // We need to call from within a process since `call` requires task-local context
        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();

        handle.spawn(move || async move {
            // Start the server
            let server_pid = start::<TestServer>(()).await.unwrap();

            let reply: String = call(server_pid, Ping, Duration::from_secs(5))
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
