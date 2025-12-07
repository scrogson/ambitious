//! # GenServer Pattern
//!
//! GenServer is a process abstraction for building stateful, message-driven actors.
//! The struct IS the process - `init` constructs it, and handlers mutate it via `&mut self`.
//!
//! # Design
//!
//! - **Typed message handlers**: `HandleCall<M>`, `HandleCast<M>`, `HandleInfo<M>` traits
//! - **Clean result types**: `Init`, `Reply`, `Status` enums
//! - **Self-as-state**: The struct implementing `GenServer` IS the state
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::*;
//! use ambitious::{call, cast, Message};
//!
//! struct Counter {
//!     count: i64,
//!     name: String,
//! }
//!
//! struct CounterArgs {
//!     initial: i64,
//!     name: String,
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
//!     type Args = CounterArgs;
//!
//!     async fn init(args: CounterArgs) -> Init<Self> {
//!         Init::Ok(Counter {
//!             count: args.initial,
//!             name: args.name,
//!         })
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
//! # Client API
//!
//! ```ignore
//! // Start the server
//! let pid = start::<Counter>(CounterArgs { initial: 0, name: "test".into() }).await?;
//!
//! // Make a typed call
//! let count: i64 = call(pid, Get, Duration::from_secs(5)).await?;
//!
//! // Send a typed cast
//! cast(pid, Increment);
//!
//! // Stop the server
//! stop(pid, ExitReason::Normal, Duration::from_secs(5)).await?;
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

pub mod dispatch;
mod protocol;
mod server;
mod traits;
mod types;

pub use async_trait::async_trait;
pub use protocol::From;
pub use server::{Error, RawReply, call, call_raw, cast, cast_raw, reply, start, start_link, stop};
pub use traits::{GenServer, HandleCall, HandleCast, HandleContinue, HandleInfo, HasHandlers};
pub use types::{Init, Reply, Status};

// Re-export core types for convenience
pub use crate::core::{ExitReason, Pid, Ref, Term};

// Re-export the register_handlers macro
pub use crate::register_handlers;

/// Prelude module for convenient imports.
///
/// Import everything needed to implement a GenServer with:
/// ```ignore
/// use ambitious::gen_server::prelude::*;
/// ```
pub mod prelude {
    pub use super::{
        Error, ExitReason, From, GenServer, HandleCall, HandleCast, HandleContinue, HandleInfo,
        HasHandlers, Init, Pid, RawReply, Reply, Status, async_trait, call, call_raw, cast,
        cast_raw, register_handlers, start, start_link, stop,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DecodeError;
    use crate::message::{Message, decode_payload, encode_payload, encode_with_tag};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    // =========================================================================
    // Counter Example - demonstrates HandleCall and HandleCast with Message trait
    // =========================================================================

    /// A simple counter GenServer.
    #[allow(dead_code)]
    struct Counter {
        count: i64,
        name: String,
    }

    /// Arguments for starting a Counter.
    #[allow(dead_code)]
    struct CounterArgs {
        initial: i64,
        name: String,
    }

    // Message types with manual Message impl
    // (derive macro generates ::ambitious:: paths which don't work inside the crate)
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Get;

    impl Message for Get {
        fn tag() -> &'static str {
            "Get"
        }
        fn encode_local(&self) -> Vec<u8> {
            encode_with_tag(Self::tag(), &[])
        }
        fn decode_local(_bytes: &[u8]) -> Result<Self, DecodeError> {
            Ok(Get)
        }
        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }
        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Increment;

    impl Message for Increment {
        fn tag() -> &'static str {
            "Increment"
        }
        fn encode_local(&self) -> Vec<u8> {
            encode_with_tag(Self::tag(), &[])
        }
        fn decode_local(_bytes: &[u8]) -> Result<Self, DecodeError> {
            Ok(Increment)
        }
        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }
        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Add(i64);

    impl Message for Add {
        fn tag() -> &'static str {
            "Add"
        }
        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(&self.0);
            encode_with_tag(Self::tag(), &payload)
        }
        fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
            let inner = decode_payload(bytes)?;
            Ok(Add(inner))
        }
        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }
        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    // Register handlers for automatic dispatch
    register_handlers!(Counter {
        calls: [Get, Add],
        casts: [Increment],
    });

    #[async_trait]
    impl GenServer for Counter {
        type Args = CounterArgs;

        async fn init(args: CounterArgs) -> Init<Self> {
            Init::Ok(Counter {
                count: args.initial,
                name: args.name,
            })
        }

        // Override to use HasHandlers dispatch
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
    impl HandleCall<Get> for Counter {
        type Reply = i64;
        type Output = Reply<i64>;

        async fn handle_call(&mut self, _msg: Get, _from: From) -> Reply<i64> {
            Reply::Ok(self.count)
        }
    }

    #[async_trait]
    impl HandleCall<Add> for Counter {
        type Reply = i64;
        type Output = Reply<i64>;

        async fn handle_call(&mut self, msg: Add, _from: From) -> Reply<i64> {
            self.count += msg.0;
            Reply::Ok(self.count)
        }
    }

    #[async_trait]
    impl HandleCast<Increment> for Counter {
        type Output = Status;

        async fn handle_cast(&mut self, _msg: Increment) -> Status {
            self.count += 1;
            Status::Ok
        }
    }

    #[tokio::test]
    async fn test_counter_call() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        // Run test in a process context
        handle.spawn(move || async move {
            // Start the counter
            let pid = start::<Counter>(CounterArgs {
                initial: 42,
                name: "test".into(),
            })
            .await
            .expect("failed to start counter");

            // Make a call to get the count using typed call
            let count: i64 = call(pid, Get, Duration::from_secs(5))
                .await
                .expect("call failed");

            result_clone.store(count, Ordering::SeqCst);
        });

        // Wait for test to complete
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(result.load(Ordering::SeqCst), 42);
    }

    #[tokio::test]
    async fn test_counter_cast_and_call() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        handle.spawn(move || async move {
            let pid = start::<Counter>(CounterArgs {
                initial: 0,
                name: "test".into(),
            })
            .await
            .expect("failed to start counter");

            // Cast increment a few times using typed cast
            cast(pid, Increment);
            cast(pid, Increment);
            cast(pid, Increment);

            // Give casts time to process
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Get the count using typed call
            let count: i64 = call(pid, Get, Duration::from_secs(5))
                .await
                .expect("call failed");

            result_clone.store(count, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(result.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_counter_add() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        handle.spawn(move || async move {
            let pid = start::<Counter>(CounterArgs {
                initial: 10,
                name: "test".into(),
            })
            .await
            .expect("failed to start counter");

            // Add 5 using typed call
            let count: i64 = call(pid, Add(5), Duration::from_secs(5))
                .await
                .expect("call failed");

            result_clone.store(count, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(result.load(Ordering::SeqCst), 15);
    }
}
