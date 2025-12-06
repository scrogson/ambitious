//! GenServer v2 - Improved actor pattern.
//!
//! This is the next generation GenServer implementation with:
//! - `&mut self` style handlers (struct IS the process)
//! - Typed message handlers (`Call<M>`, `Cast<M>`, `Info<M>`)
//! - Clean result types (`Init`, `Reply`, `Status`)
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::v2::*;
//! use std::time::Duration;
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
//! struct Get;
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
//! impl Call<Get> for Counter {
//!     type Reply = i64;
//!
//!     async fn call(&mut self, _msg: Get, _from: From) -> Reply<i64> {
//!         Reply::Ok(self.count)
//!     }
//! }
//!
//! impl Cast<Increment> for Counter {
//!     async fn cast(&mut self, _msg: Increment) -> Status {
//!         self.count += 1;
//!         Status::Ok
//!     }
//! }
//! ```

mod protocol;
mod server;
mod traits;
mod types;

pub use protocol::From;
pub use server::{Error, RawReply, call, call_raw, cast, cast_raw, reply, start, start_link, stop};
pub use traits::{Call, Cast, GenServer, Info, async_trait};
pub use types::{Init, Reply, Status};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{DecodeError, ExitReason};
    use crate::message::{Message, decode_payload, decode_tag, encode_payload, encode_with_tag};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    // =========================================================================
    // Counter Example - demonstrates Call and Cast with Message trait
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

    #[async_trait]
    impl GenServer for Counter {
        type Args = CounterArgs;

        async fn init(args: CounterArgs) -> Init<Self> {
            Init::Ok(Counter {
                count: args.initial,
                name: args.name,
            })
        }

        async fn handle_call_raw(&mut self, payload: Vec<u8>, from: From) -> RawReply {
            // Decode the tag to determine message type
            let (tag, msg_payload) = match decode_tag(&payload) {
                Ok((t, p)) => (t, p),
                Err(_) => {
                    return RawReply::StopNoReply(ExitReason::Error("decode error".into()));
                }
            };

            // Dispatch based on tag
            match tag {
                "Get" => {
                    let result = self.call(Get, from).await;
                    reply_to_raw(result)
                }
                "Add" => {
                    if let Ok(msg) = Add::decode_local(msg_payload) {
                        let result = self.call(msg, from).await;
                        reply_to_raw(result)
                    } else {
                        RawReply::StopNoReply(ExitReason::Error("decode error".into()))
                    }
                }
                _ => RawReply::StopNoReply(ExitReason::Error(format!("unknown call: {}", tag))),
            }
        }

        async fn handle_cast_raw(&mut self, payload: Vec<u8>) -> Status {
            // Decode the tag to determine message type
            let (tag, _msg_payload) = match decode_tag(&payload) {
                Ok((t, p)) => (t, p),
                Err(_) => {
                    return Status::Stop(ExitReason::Error("decode error".into()));
                }
            };

            match tag {
                "Increment" => self.cast(Increment).await,
                _ => Status::Ok, // Ignore unknown casts
            }
        }
    }

    // Helper to convert Reply<T> to RawReply
    fn reply_to_raw<T: Message>(reply: Reply<T>) -> RawReply {
        match reply {
            Reply::Ok(v) => RawReply::Ok(v.encode_local()),
            Reply::Continue(v, arg) => RawReply::Continue(v.encode_local(), arg),
            Reply::Timeout(v, d) => RawReply::Timeout(v.encode_local(), d),
            Reply::NoReply => RawReply::NoReply,
            Reply::Stop(r, v) => RawReply::Stop(r, v.encode_local()),
            Reply::StopNoReply(r) => RawReply::StopNoReply(r),
        }
    }

    #[async_trait]
    impl Call<Get> for Counter {
        type Reply = i64;

        async fn call(&mut self, _msg: Get, _from: From) -> Reply<i64> {
            Reply::Ok(self.count)
        }
    }

    #[async_trait]
    impl Call<Add> for Counter {
        type Reply = i64;

        async fn call(&mut self, msg: Add, _from: From) -> Reply<i64> {
            self.count += msg.0;
            Reply::Ok(self.count)
        }
    }

    #[async_trait]
    impl Cast<Increment> for Counter {
        async fn cast(&mut self, _msg: Increment) -> Status {
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
