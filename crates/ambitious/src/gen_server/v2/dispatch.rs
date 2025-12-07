//! Automatic message dispatch using linkme for handler registration.
//!
//! This module provides the infrastructure for automatically dispatching
//! messages to the correct handler without requiring manual `handle_call_raw`
//! implementations or the `register_handlers!` macro.
//!
//! # How It Works
//!
//! 1. Each `Call<M>`, `Cast<M>`, `Info<M>` impl is annotated with an attribute
//!    macro (`#[call]`, `#[cast]`, `#[info]`) that registers a handler entry.
//!
//! 2. Handler entries are collected at link time using the `linkme` crate.
//!
//! 3. When a message arrives, the gen_server loop looks up the handler by
//!    TypeId + message tag and dispatches to the correct typed handler.
//!
//! # Example
//!
//! ```ignore
//! use ambitious::gen_server::v2::*;
//! use ambitious::Message;
//!
//! #[derive(Message)]
//! struct GetCount;
//!
//! struct Counter { count: i64 }
//!
//! #[async_trait]
//! impl GenServer for Counter {
//!     type Args = i64;
//!     async fn init(initial: i64) -> Init<Self> {
//!         Init::Ok(Counter { count: initial })
//!     }
//! }
//!
//! // The #[call] attribute registers this handler automatically
//! #[call]
//! #[async_trait]
//! impl Call<GetCount> for Counter {
//!     type Reply = i64;
//!     type Output = Reply<i64>;
//!
//!     async fn call(&mut self, _msg: GetCount, _from: From) -> Reply<i64> {
//!         Reply::Ok(self.count)
//!     }
//! }
//! ```

use super::protocol::From;
use super::server::RawReply;
use super::types::Status;
use crate::core::ExitReason;
use crate::message::Message;
use linkme::distributed_slice;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

// =============================================================================
// Type-erased dispatch functions (used with dyn Any)
// =============================================================================

/// Type-erased async dispatch function for call handlers.
/// Takes `&mut dyn Any + Send` which will be downcast to the actual server type.
pub type AnyCallDispatchFn = for<'a> fn(
    &'a mut (dyn Any + Send),
    &'a [u8],
    From,
) -> Pin<Box<dyn Future<Output = RawReply> + Send + 'a>>;

/// Type-erased async dispatch function for cast handlers.
pub type AnyCastDispatchFn =
    for<'a> fn(&'a mut (dyn Any + Send), &'a [u8]) -> Pin<Box<dyn Future<Output = Status> + Send + 'a>>;

/// Type-erased async dispatch function for info handlers.
pub type AnyInfoDispatchFn =
    for<'a> fn(&'a mut (dyn Any + Send), &'a [u8]) -> Pin<Box<dyn Future<Output = Status> + Send + 'a>>;

// =============================================================================
// Handler Entry Types (stored in distributed slices)
// =============================================================================

/// A registered call handler entry.
///
/// This is collected via linkme's distributed slice mechanism.
pub struct CallHandlerEntry {
    /// Returns the TypeId of the GenServer this handler is for.
    pub server_type_id: fn() -> TypeId,
    /// Returns the message tag this handler responds to.
    pub msg_tag: fn() -> &'static str,
    /// The dispatch function.
    pub dispatch: AnyCallDispatchFn,
}

// Safety: Contains only function pointers
unsafe impl Send for CallHandlerEntry {}
unsafe impl Sync for CallHandlerEntry {}

/// A registered cast handler entry.
pub struct CastHandlerEntry {
    /// Returns the TypeId of the GenServer this handler is for.
    pub server_type_id: fn() -> TypeId,
    /// Returns the message tag this handler responds to.
    pub msg_tag: fn() -> &'static str,
    /// The dispatch function.
    pub dispatch: AnyCastDispatchFn,
}

unsafe impl Send for CastHandlerEntry {}
unsafe impl Sync for CastHandlerEntry {}

/// A registered info handler entry.
pub struct InfoHandlerEntry {
    /// Returns the TypeId of the GenServer this handler is for.
    pub server_type_id: fn() -> TypeId,
    /// Returns the message tag this handler responds to.
    pub msg_tag: fn() -> &'static str,
    /// The dispatch function.
    pub dispatch: AnyInfoDispatchFn,
}

unsafe impl Send for InfoHandlerEntry {}
unsafe impl Sync for InfoHandlerEntry {}

// =============================================================================
// Distributed Slices (populated by #[call], #[cast], #[info] macros)
// =============================================================================

/// Distributed slice for call handlers, populated at link time.
#[distributed_slice]
pub static CALL_HANDLERS: [CallHandlerEntry];

/// Distributed slice for cast handlers, populated at link time.
#[distributed_slice]
pub static CAST_HANDLERS: [CastHandlerEntry];

/// Distributed slice for info handlers, populated at link time.
#[distributed_slice]
pub static INFO_HANDLERS: [InfoHandlerEntry];

// =============================================================================
// Global Handler Registry
// =============================================================================

/// Key for looking up handlers: (ServerTypeId, MessageTag)
type HandlerKey = (TypeId, &'static str);

/// Global registry of all handlers, built lazily from distributed slices.
struct GlobalRegistry {
    calls: HashMap<HandlerKey, AnyCallDispatchFn>,
    casts: HashMap<HandlerKey, AnyCastDispatchFn>,
    infos: HashMap<HandlerKey, AnyInfoDispatchFn>,
}

impl GlobalRegistry {
    fn build() -> Self {
        let mut calls = HashMap::new();
        let mut casts = HashMap::new();
        let mut infos = HashMap::new();

        for entry in CALL_HANDLERS {
            let key = ((entry.server_type_id)(), (entry.msg_tag)());
            calls.insert(key, entry.dispatch);
        }

        for entry in CAST_HANDLERS {
            let key = ((entry.server_type_id)(), (entry.msg_tag)());
            casts.insert(key, entry.dispatch);
        }

        for entry in INFO_HANDLERS {
            let key = ((entry.server_type_id)(), (entry.msg_tag)());
            infos.insert(key, entry.dispatch);
        }

        GlobalRegistry {
            calls,
            casts,
            infos,
        }
    }
}

static GLOBAL_REGISTRY: OnceLock<GlobalRegistry> = OnceLock::new();

fn global_registry() -> &'static GlobalRegistry {
    GLOBAL_REGISTRY.get_or_init(GlobalRegistry::build)
}

// =============================================================================
// Dispatch Functions (called by the gen_server loop)
// =============================================================================

/// Dispatch a call message to the appropriate handler.
///
/// This is the main entry point for call dispatch. It looks up the handler
/// in the global registry using the server's TypeId and message tag.
///
/// Returns `Some(reply)` if a handler was found, `None` otherwise.
pub async fn dispatch_call<G: Send + 'static>(
    server: &mut G,
    tag: &str,
    payload: &[u8],
    from: From,
) -> Option<RawReply> {
    let registry = global_registry();
    let type_id = TypeId::of::<G>();

    // We need to find a handler with a matching tag (static str comparison)
    // Since we store &'static str, we need to iterate to find a match
    let dispatch = registry.calls.iter().find_map(|((tid, msg_tag), dispatch)| {
        if *tid == type_id && *msg_tag == tag {
            Some(*dispatch)
        } else {
            None
        }
    });

    if let Some(dispatch) = dispatch {
        Some(dispatch(server as &mut (dyn Any + Send), payload, from).await)
    } else {
        None
    }
}

/// Dispatch a cast message to the appropriate handler.
///
/// Returns `Some(status)` if a handler was found, `None` otherwise.
pub async fn dispatch_cast<G: Send + 'static>(server: &mut G, tag: &str, payload: &[u8]) -> Option<Status> {
    let registry = global_registry();
    let type_id = TypeId::of::<G>();

    let dispatch = registry.casts.iter().find_map(|((tid, msg_tag), dispatch)| {
        if *tid == type_id && *msg_tag == tag {
            Some(*dispatch)
        } else {
            None
        }
    });

    if let Some(dispatch) = dispatch {
        Some(dispatch(server as &mut (dyn Any + Send), payload).await)
    } else {
        None
    }
}

/// Dispatch an info message to the appropriate handler.
///
/// Returns `Some(status)` if a handler was found, `None` otherwise.
pub async fn dispatch_info<G: Send + 'static>(server: &mut G, tag: &str, payload: &[u8]) -> Option<Status> {
    let registry = global_registry();
    let type_id = TypeId::of::<G>();

    let dispatch = registry.infos.iter().find_map(|((tid, msg_tag), dispatch)| {
        if *tid == type_id && *msg_tag == tag {
            Some(*dispatch)
        } else {
            None
        }
    });

    if let Some(dispatch) = dispatch {
        Some(dispatch(server as &mut (dyn Any + Send), payload).await)
    } else {
        None
    }
}

// =============================================================================
// Legacy Support: Typed Handler Registry (for register_handlers! macro)
// =============================================================================

/// Type-erased async dispatch function for call handlers (typed version).
pub type CallDispatchFn<G> =
    for<'a> fn(&'a mut G, &'a [u8], From) -> Pin<Box<dyn Future<Output = RawReply> + Send + 'a>>;

/// Type-erased async dispatch function for cast handlers (typed version).
pub type CastDispatchFn<G> =
    for<'a> fn(&'a mut G, &'a [u8]) -> Pin<Box<dyn Future<Output = Status> + Send + 'a>>;

/// Type-erased async dispatch function for info handlers (typed version).
pub type InfoDispatchFn<G> =
    for<'a> fn(&'a mut G, &'a [u8]) -> Pin<Box<dyn Future<Output = Status> + Send + 'a>>;

/// A registered call handler (typed version).
pub struct CallHandler<G: 'static> {
    /// The message tag this handler responds to.
    pub tag: &'static str,
    /// The dispatch function.
    pub dispatch: CallDispatchFn<G>,
}

unsafe impl<G: 'static> Send for CallHandler<G> {}
unsafe impl<G: 'static> Sync for CallHandler<G> {}

/// A registered cast handler (typed version).
pub struct CastHandler<G: 'static> {
    /// The message tag this handler responds to.
    pub tag: &'static str,
    /// The dispatch function.
    pub dispatch: CastDispatchFn<G>,
}

unsafe impl<G: 'static> Send for CastHandler<G> {}
unsafe impl<G: 'static> Sync for CastHandler<G> {}

/// A registered info handler (typed version).
pub struct InfoHandler<G: 'static> {
    /// The message tag this handler responds to.
    pub tag: &'static str,
    /// The dispatch function.
    pub dispatch: InfoDispatchFn<G>,
}

unsafe impl<G: 'static> Send for InfoHandler<G> {}
unsafe impl<G: 'static> Sync for InfoHandler<G> {}

/// Registry of handlers for a specific GenServer type.
///
/// This is used by the `register_handlers!` macro for backwards compatibility.
pub struct HandlerRegistry<G: 'static> {
    calls: HashMap<&'static str, CallDispatchFn<G>>,
    casts: HashMap<&'static str, CastDispatchFn<G>>,
    infos: HashMap<&'static str, InfoDispatchFn<G>>,
}

impl<G: 'static> HandlerRegistry<G> {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            calls: HashMap::new(),
            casts: HashMap::new(),
            infos: HashMap::new(),
        }
    }

    /// Register a call handler.
    pub fn register_call(&mut self, tag: &'static str, dispatch: CallDispatchFn<G>) {
        self.calls.insert(tag, dispatch);
    }

    /// Register a cast handler.
    pub fn register_cast(&mut self, tag: &'static str, dispatch: CastDispatchFn<G>) {
        self.casts.insert(tag, dispatch);
    }

    /// Register an info handler.
    pub fn register_info(&mut self, tag: &'static str, dispatch: InfoDispatchFn<G>) {
        self.infos.insert(tag, dispatch);
    }

    /// Dispatch a call message to the appropriate handler.
    pub async fn dispatch_call(
        &self,
        server: &mut G,
        tag: &str,
        payload: &[u8],
        from: From,
    ) -> RawReply {
        if let Some(dispatch) = self.calls.get(tag) {
            dispatch(server, payload, from).await
        } else {
            RawReply::StopNoReply(ExitReason::Error(format!("unknown call: {}", tag)))
        }
    }

    /// Dispatch a cast message to the appropriate handler.
    pub async fn dispatch_cast(&self, server: &mut G, tag: &str, payload: &[u8]) -> Status {
        if let Some(dispatch) = self.casts.get(tag) {
            dispatch(server, payload).await
        } else {
            Status::Ok
        }
    }

    /// Dispatch an info message to the appropriate handler.
    pub async fn dispatch_info(&self, server: &mut G, tag: &str, payload: &[u8]) -> Status {
        if let Some(dispatch) = self.infos.get(tag) {
            dispatch(server, payload).await
        } else {
            Status::Ok
        }
    }
}

impl<G: 'static> Default for HandlerRegistry<G> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Helper to convert a Reply<T> to RawReply.
///
/// This is used by the dispatch code to encode replies.
pub fn reply_to_raw<T: Message>(reply: super::types::Reply<T>) -> RawReply {
    use super::types::Reply;
    match reply {
        Reply::Ok(v) => RawReply::Ok(v.encode_local()),
        Reply::Continue(v, arg) => RawReply::Continue(v.encode_local(), arg),
        Reply::Timeout(v, d) => RawReply::Timeout(v.encode_local(), d),
        Reply::NoReply => RawReply::NoReply,
        Reply::Stop(r, v) => RawReply::Stop(r, v.encode_local()),
        Reply::StopNoReply(r) => RawReply::StopNoReply(r),
    }
}

// =============================================================================
// Legacy Macros (for backwards compatibility with register_handlers!)
// =============================================================================

/// Macro to create a dispatch function for a Call handler.
///
/// This is used by the `register_handlers!` macro.
#[macro_export]
macro_rules! call_dispatch_fn {
    ($server:ty, $msg:ty, $reply:ty) => {
        |server: &mut $server, payload: &[u8], from: $crate::gen_server::v2::From| {
            Box::pin(async move {
                use $crate::gen_server::v2::Call;
                use $crate::message::Message;

                let msg = match <$msg>::decode_local(payload) {
                    Ok(m) => m,
                    Err(e) => {
                        return $crate::gen_server::v2::RawReply::StopNoReply(
                            $crate::core::ExitReason::Error(format!("decode error: {}", e)),
                        );
                    }
                };

                let result = <$server as Call<$msg>>::call(server, msg, from).await;
                let reply: $crate::gen_server::v2::Reply<_> = result.into();
                $crate::gen_server::v2::dispatch::reply_to_raw(reply)
            })
        }
    };
}

/// Macro to create a dispatch function for a Cast handler.
#[macro_export]
macro_rules! cast_dispatch_fn {
    ($server:ty, $msg:ty) => {
        |server: &mut $server, payload: &[u8]| {
            Box::pin(async move {
                use $crate::gen_server::v2::Cast;
                use $crate::message::Message;

                let msg = match <$msg>::decode_local(payload) {
                    Ok(m) => m,
                    Err(e) => {
                        return $crate::gen_server::v2::Status::Stop(
                            $crate::core::ExitReason::Error(format!("decode error: {}", e)),
                        );
                    }
                };

                let result = <$server as Cast<$msg>>::cast(server, msg).await;
                result.into()
            })
        }
    };
}

/// Macro to create a dispatch function for an Info handler.
#[macro_export]
macro_rules! info_dispatch_fn {
    ($server:ty, $msg:ty) => {
        |server: &mut $server, payload: &[u8]| {
            Box::pin(async move {
                use $crate::gen_server::v2::Info;
                use $crate::message::Message;

                let msg = match <$msg>::decode_local(payload) {
                    Ok(m) => m,
                    Err(e) => {
                        return $crate::gen_server::v2::Status::Stop(
                            $crate::core::ExitReason::Error(format!("decode error: {}", e)),
                        );
                    }
                };

                let result = <$server as Info<$msg>>::info(server, msg).await;
                result.into()
            })
        }
    };
}

/// Macro to register handlers for a GenServer.
///
/// This should be called once per GenServer type, listing all message types.
/// The macro generates implementations that override the default `GenServer`
/// methods for message dispatch.
///
/// # Example
///
/// ```ignore
/// register_handlers!(Room {
///     calls: [GetHistory],
///     casts: [StoreMessage],
/// });
/// ```
///
/// With this macro, you don't need to implement `handle_call_raw` or `handle_cast_raw`
/// manually. Just implement `GenServer::init` and your `Call<M>`, `Cast<M>`, `Info<M>` handlers.
///
/// **Note:** Consider using the `#[call]`, `#[cast]`, `#[info]` attribute macros instead,
/// which eliminate the need for this macro entirely.
#[macro_export]
macro_rules! register_handlers {
    ($server:ty {
        $(calls: [$($call_msg:ty),* $(,)?],)?
        $(casts: [$($cast_msg:ty),* $(,)?],)?
        $(infos: [$($info_msg:ty),* $(,)?],)?
    }) => {
        // Implement HasHandlers for legacy compatibility
        impl $crate::gen_server::v2::HasHandlers for $server {
            fn registry() -> &'static $crate::gen_server::v2::dispatch::HandlerRegistry<Self> {
                use $crate::gen_server::v2::dispatch::HandlerRegistry;
                use $crate::message::Message;
                use std::sync::OnceLock;

                static REGISTRY: OnceLock<HandlerRegistry<$server>> = OnceLock::new();

                REGISTRY.get_or_init(|| {
                    let mut registry = HandlerRegistry::new();

                    $($(
                        registry.register_call(
                            <$call_msg>::tag(),
                            $crate::call_dispatch_fn!($server, $call_msg, <$server as $crate::gen_server::v2::Call<$call_msg>>::Reply)
                        );
                    )*)?

                    $($(
                        registry.register_cast(
                            <$cast_msg>::tag(),
                            $crate::cast_dispatch_fn!($server, $cast_msg)
                        );
                    )*)?

                    $($(
                        registry.register_info(
                            <$info_msg>::tag(),
                            $crate::info_dispatch_fn!($server, $info_msg)
                        );
                    )*)?

                    registry
                })
            }

            fn handle_call_dispatch(
                server: &mut Self,
                payload: Vec<u8>,
                from: $crate::gen_server::v2::From,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = $crate::gen_server::v2::RawReply> + Send + '_>> {
                Box::pin(async move {
                    use $crate::message::decode_tag;
                    let (tag, msg_payload) = match decode_tag(&payload) {
                        Ok((t, p)) => (t, p),
                        Err(_) => {
                            return $crate::gen_server::v2::RawReply::StopNoReply(
                                $crate::core::ExitReason::Error("decode error".into())
                            );
                        }
                    };
                    <Self as $crate::gen_server::v2::HasHandlers>::registry()
                        .dispatch_call(server, tag, msg_payload, from)
                        .await
                })
            }

            fn handle_cast_dispatch(
                server: &mut Self,
                payload: Vec<u8>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = $crate::gen_server::v2::Status> + Send + '_>> {
                Box::pin(async move {
                    use $crate::message::decode_tag;
                    let (tag, msg_payload) = match decode_tag(&payload) {
                        Ok((t, p)) => (t, p),
                        Err(_) => {
                            return $crate::gen_server::v2::Status::Stop(
                                $crate::core::ExitReason::Error("decode error".into())
                            );
                        }
                    };
                    <Self as $crate::gen_server::v2::HasHandlers>::registry()
                        .dispatch_cast(server, tag, msg_payload)
                        .await
                })
            }

            fn handle_info_dispatch(
                server: &mut Self,
                payload: Vec<u8>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = $crate::gen_server::v2::Status> + Send + '_>> {
                Box::pin(async move {
                    use $crate::message::decode_tag;
                    let (tag, msg_payload) = match decode_tag(&payload) {
                        Ok((t, p)) => (t, p),
                        Err(_) => {
                            return $crate::gen_server::v2::Status::Ok;
                        }
                    };
                    <Self as $crate::gen_server::v2::HasHandlers>::registry()
                        .dispatch_info(server, tag, msg_payload)
                        .await
                })
            }
        }

        // Override GenServer handle_*_raw methods to use the registry
        impl $server {
            /// Handle a raw call message using the registered handlers.
            pub async fn _gen_server_handle_call_raw(
                &mut self,
                payload: Vec<u8>,
                from: $crate::gen_server::v2::From,
            ) -> $crate::gen_server::v2::RawReply {
                <Self as $crate::gen_server::v2::HasHandlers>::handle_call_dispatch(self, payload, from).await
            }

            /// Handle a raw cast message using the registered handlers.
            pub async fn _gen_server_handle_cast_raw(
                &mut self,
                payload: Vec<u8>,
            ) -> $crate::gen_server::v2::Status {
                <Self as $crate::gen_server::v2::HasHandlers>::handle_cast_dispatch(self, payload).await
            }

            /// Handle a raw info message using the registered handlers.
            pub async fn _gen_server_handle_info_raw(
                &mut self,
                payload: Vec<u8>,
            ) -> $crate::gen_server::v2::Status {
                <Self as $crate::gen_server::v2::HasHandlers>::handle_info_dispatch(self, payload).await
            }
        }
    };
}

pub use register_handlers;
