//! # GenServer Pattern
//!
//! GenServer is a process abstraction for building stateful, message-driven actors.
//! The struct IS the process - `init` constructs it, and handlers mutate it via `&mut self`.
//!
//! # Design
//!
//! - **Enum-based message dispatch**: Define `Call`, `Cast`, `Info` message enums
//! - **OTP envelope compatibility**: Messages wrapped in `$gen_call`/`$gen_cast` format
//! - **Self-as-state**: The struct implementing `GenServer` IS the state
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::*;
//!
//! struct Counter {
//!     count: i64,
//! }
//!
//! #[derive(Message)]
//! enum CounterCall {
//!     Get,
//!     Reset(i64),
//! }
//!
//! #[derive(Message)]
//! enum CounterCast {
//!     Increment,
//!     Decrement,
//! }
//!
//! impl GenServer for Counter {
//!     type Args = i64;
//!     type Call = CounterCall;
//!     type Cast = CounterCast;
//!     type Info = ();
//!     type Reply = i64;
//!
//!     async fn init(initial: i64) -> Init<Self> {
//!         Init::Ok(Counter { count: initial })
//!     }
//!
//!     async fn handle_call(&mut self, msg: CounterCall, _from: From) -> Reply<i64> {
//!         match msg {
//!             CounterCall::Get => Reply::Ok(self.count),
//!             CounterCall::Reset(val) => {
//!                 let old = self.count;
//!                 self.count = val;
//!                 Reply::Ok(old)
//!             }
//!         }
//!     }
//!
//!     async fn handle_cast(&mut self, msg: CounterCast) -> Status {
//!         match msg {
//!             CounterCast::Increment => self.count += 1,
//!             CounterCast::Decrement => self.count -= 1,
//!         }
//!         Status::Ok
//!     }
//!
//!     async fn handle_info(&mut self, _msg: ()) -> Status {
//!         Status::Ok
//!     }
//! }
//! ```
//!
//! # Client API
//!
//! ```ignore
//! // Start the server
//! let pid = start::<Counter>(0).await?;
//!
//! // Make a typed call
//! let count: i64 = call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5)).await?;
//!
//! // Send a typed cast
//! cast::<Counter>(pid, CounterCast::Increment);
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

pub(crate) mod protocol;
mod server;
mod traits;
mod types;

// Primary exports
pub use server::{Error, call, cast, start, start_link};
pub use traits::GenServer;

pub use async_trait::async_trait;
pub use protocol::From;
pub use types::{Init, Reply, Status};

// Re-export core types for convenience
pub use crate::core::{ExitReason, Pid, Ref, Term};

/// Prelude module for convenient imports.
///
/// Import everything needed to implement a GenServer with:
/// ```ignore
/// use ambitious::gen_server::prelude::*;
/// ```
pub mod prelude {
    pub use super::{
        Error, ExitReason, From, GenServer, Init, Pid, Reply, Status, async_trait, call, cast,
        start, start_link,
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
    // Counter Example - demonstrates the enum-based approach
    // =========================================================================

    /// A simple counter GenServer.
    struct Counter {
        count: i64,
    }

    /// All call messages for the counter.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum CounterCall {
        Get,
        Reset(i64),
    }

    impl Message for CounterCall {
        const TAG: &'static str = "CounterCall";

        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(self);
            encode_with_tag(Self::TAG, &payload)
        }
        fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
            decode_payload(bytes)
        }
        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }
        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    /// All cast messages for the counter.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum CounterCast {
        Increment,
        Decrement,
    }

    impl Message for CounterCast {
        const TAG: &'static str = "CounterCast";

        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(self);
            encode_with_tag(Self::TAG, &payload)
        }
        fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
            decode_payload(bytes)
        }
        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }
        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    /// Reply type for counter calls.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct CounterReply(i64);

    impl Message for CounterReply {
        const TAG: &'static str = "CounterReply";

        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(&self.0);
            encode_with_tag(Self::TAG, &payload)
        }
        fn decode_local(bytes: &[u8]) -> Result<Self, DecodeError> {
            let inner = decode_payload(bytes)?;
            Ok(CounterReply(inner))
        }
        fn encode_remote(&self) -> Vec<u8> {
            self.encode_local()
        }
        fn decode_remote(bytes: &[u8]) -> Result<Self, DecodeError> {
            Self::decode_local(bytes)
        }
    }

    #[async_trait]
    impl GenServer for Counter {
        type Args = i64;
        type Call = CounterCall;
        type Cast = CounterCast;
        type Info = (); // No info messages
        type Reply = CounterReply;

        async fn init(initial: i64) -> Init<Self> {
            Init::Ok(Counter { count: initial })
        }

        async fn handle_call(&mut self, msg: CounterCall, _from: From) -> Reply<CounterReply> {
            match msg {
                CounterCall::Get => Reply::Ok(CounterReply(self.count)),
                CounterCall::Reset(val) => {
                    let old = self.count;
                    self.count = val;
                    Reply::Ok(CounterReply(old))
                }
            }
        }

        async fn handle_cast(&mut self, msg: CounterCast) -> Status {
            match msg {
                CounterCast::Increment => self.count += 1,
                CounterCast::Decrement => self.count -= 1,
            }
            Status::Ok
        }

        async fn handle_info(&mut self, _msg: ()) -> Status {
            Status::Ok
        }
    }

    #[tokio::test]
    async fn test_counter_call() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        handle.spawn(move || async move {
            let pid = start::<Counter>(42).await.expect("failed to start counter");

            let reply: CounterReply =
                call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5))
                    .await
                    .expect("call failed");

            result_clone.store(reply.0, Ordering::SeqCst);
        });

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
            let pid = start::<Counter>(0).await.expect("failed to start counter");

            // Cast increment a few times
            cast::<Counter>(pid, CounterCast::Increment);
            cast::<Counter>(pid, CounterCast::Increment);
            cast::<Counter>(pid, CounterCast::Increment);

            // Give casts time to process
            tokio::time::sleep(Duration::from_millis(50)).await;

            let reply: CounterReply =
                call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5))
                    .await
                    .expect("call failed");

            result_clone.store(reply.0, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(result.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_counter_reset() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        handle.spawn(move || async move {
            let pid = start::<Counter>(10).await.expect("failed to start counter");

            // Reset to 100, should return old value (10)
            let reply: CounterReply =
                call::<Counter, _>(pid, CounterCall::Reset(100), Duration::from_secs(5))
                    .await
                    .expect("call failed");

            result_clone.store(reply.0, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(result.load(Ordering::SeqCst), 10);
    }
}
