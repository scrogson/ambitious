//! GenServer v3 - Enum-based message dispatch with OTP envelope compatibility.
//!
//! This is a redesign of the GenServer pattern that:
//! - Uses associated enum types instead of per-message traits
//! - Wraps messages in OTP-compatible envelopes (`$gen_call`, `$gen_cast`)
//! - Enables users to easily create custom behaviors
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::v3::*;
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
//!     type Info = ();  // No info messages
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

mod traits;
mod server;

pub use traits::GenServer;
pub use server::{start, start_link, call, cast, Error};

// Re-export types from parent module
pub use super::protocol::From;
pub use super::types::{Init, Reply, Status};
pub use crate::core::ExitReason;
pub use async_trait::async_trait;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DecodeError;
    use crate::message::{Message, encode_with_tag, encode_payload, decode_payload};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    // =========================================================================
    // Counter Example - demonstrates the new v3 enum-based approach
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
        fn tag() -> &'static str {
            "CounterCall"
        }
        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(self);
            encode_with_tag(Self::tag(), &payload)
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
        fn tag() -> &'static str {
            "CounterCast"
        }
        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(self);
            encode_with_tag(Self::tag(), &payload)
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
        fn tag() -> &'static str {
            "CounterReply"
        }
        fn encode_local(&self) -> Vec<u8> {
            let payload = encode_payload(&self.0);
            encode_with_tag(Self::tag(), &payload)
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
        type Info = ();  // No info messages
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
    async fn test_v3_counter_call() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        handle.spawn(move || async move {
            let pid = start::<Counter>(42).await.expect("failed to start counter");

            let reply: CounterReply = call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5))
                .await
                .expect("call failed");

            result_clone.store(reply.0, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(result.load(Ordering::SeqCst), 42);
    }

    #[tokio::test]
    async fn test_v3_counter_cast_and_call() {
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

            let reply: CounterReply = call::<Counter, _>(pid, CounterCall::Get, Duration::from_secs(5))
                .await
                .expect("call failed");

            result_clone.store(reply.0, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(result.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_v3_counter_reset() {
        crate::init();
        let handle = crate::handle();

        let result = Arc::new(AtomicI64::new(-1));
        let result_clone = result.clone();

        handle.spawn(move || async move {
            let pid = start::<Counter>(10).await.expect("failed to start counter");

            // Reset to 100, should return old value (10)
            let reply: CounterReply = call::<Counter, _>(pid, CounterCall::Reset(100), Duration::from_secs(5))
                .await
                .expect("call failed");

            result_clone.store(reply.0, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(result.load(Ordering::SeqCst), 10);
    }
}
