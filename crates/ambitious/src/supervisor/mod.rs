//! # ambitious-supervisor
//!
//! Supervisor pattern implementation for Ambitious.
//!
//! This crate provides the `Supervisor` trait and related types for building
//! fault-tolerant supervision trees, mirroring Elixir's Supervisor behavior.
//!
//! # Overview
//!
//! A Supervisor is a process that monitors other processes (children) and
//! restarts them when they fail. Supervisors can be arranged in a tree
//! structure to build fault-tolerant systems.
//!
//! # Supervision Strategies
//!
//! - **OneForOne**: If a child terminates, only that child is restarted.
//! - **OneForAll**: If any child terminates, all children are restarted.
//! - **RestForOne**: If a child terminates, that child and all children
//!   started after it are restarted.
//!
//! # Restart Types
//!
//! - **Permanent**: Always restart the child.
//! - **Transient**: Only restart if the child terminates abnormally.
//! - **Temporary**: Never restart the child.
//!
//! # Example
//!
//! ```ignore
//! use ambitious_supervisor::{Supervisor, SupervisorInit, SupervisorFlags, ChildSpec, Strategy};
//!
//! struct MySupervisor;
//!
//! impl Supervisor for MySupervisor {
//!     type InitArg = ();
//!
//!     fn init(_arg: ()) -> SupervisorInit {
//!         SupervisorInit::new(
//!             SupervisorFlags::new(Strategy::OneForOne)
//!                 .max_restarts(3)
//!                 .max_seconds(5),
//!             vec![
//!                 // Child specifications go here
//!             ],
//!         )
//!     }
//! }
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

mod core;
mod error;
mod types;

/// DynamicSupervisor for starting children on demand.
///
/// Unlike regular supervisors, a DynamicSupervisor starts with no children
/// and children are added dynamically via `start_child`. It only supports
/// the one-for-one strategy.
///
/// See [`dynamic_supervisor`] module for details.
pub mod dynamic_supervisor;

pub use core::{
    Supervisor, SupervisorInit, count_children, delete_child, start, start_link, terminate_child,
    which_children,
};
pub use error::{DeleteError, RestartError, StartError, TerminateError};
pub use types::{
    ChildCounts, ChildInfo, ChildSpec, ChildType, RestartType, ShutdownType, StartChildError,
    Strategy, SupervisorFlags,
};

// Re-export commonly used types
pub use crate::core::{ExitReason, Pid, Term};
pub use crate::process::RuntimeHandle;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Runtime;
    use std::time::Duration;
    use tokio::time::sleep;

    struct TestSupervisor;

    impl Supervisor for TestSupervisor {
        type InitArg = ();

        fn init(_arg: ()) -> SupervisorInit {
            SupervisorInit::new(
                SupervisorFlags::new(Strategy::OneForOne)
                    .max_restarts(3)
                    .max_seconds(5),
                vec![], // No children for basic test
            )
        }
    }

    #[tokio::test]
    async fn test_start_supervisor() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        let pid = start::<TestSupervisor>(&handle, ()).await.unwrap();
        assert!(handle.alive(pid));

        sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_supervisor_flags() {
        let flags = SupervisorFlags::new(Strategy::OneForAll)
            .max_restarts(5)
            .max_seconds(10);

        assert_eq!(flags.strategy, Strategy::OneForAll);
        assert_eq!(flags.max_restarts, 5);
        assert_eq!(flags.max_seconds, 10);
    }

    #[tokio::test]
    async fn test_child_spec_builder() {
        let spec = ChildSpec::new("test_child", || async { Err(StartChildError::Ignore) })
            .restart(RestartType::Transient)
            .shutdown(ShutdownType::Timeout(Duration::from_secs(10)))
            .worker();

        assert_eq!(spec.id, "test_child");
        assert_eq!(spec.restart, RestartType::Transient);
        assert!(matches!(spec.shutdown, ShutdownType::Timeout(_)));
        assert_eq!(spec.child_type, ChildType::Worker);
    }

    #[tokio::test]
    async fn test_strategy_default() {
        assert_eq!(Strategy::default(), Strategy::OneForOne);
    }

    #[tokio::test]
    async fn test_restart_type_default() {
        assert_eq!(RestartType::default(), RestartType::Permanent);
    }

    #[tokio::test]
    async fn test_supervisor_with_child() {
        let runtime = Runtime::new();
        let handle = runtime.handle();

        // Create a supervisor with a simple child that runs forever
        struct WorkerSupervisor;

        impl Supervisor for WorkerSupervisor {
            type InitArg = RuntimeHandle;

            fn init(handle: Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne),
                    vec![ChildSpec::new("worker", move || {
                        let h = handle_clone.clone();
                        async move {
                            let pid = h.spawn(|| async move {
                                // Simple worker that waits for messages
                                while let Ok(Some(_)) =
                                    crate::runtime::recv_timeout(Duration::from_secs(60)).await
                                {
                                }
                            });
                            Ok(pid)
                        }
                    })],
                )
            }
        }

        let sup_pid = start::<WorkerSupervisor>(&handle, handle.clone())
            .await
            .unwrap();
        assert!(handle.alive(sup_pid));

        // Give it time to start children
        sleep(Duration::from_millis(100)).await;

        // Supervisor should still be running
        assert!(handle.alive(sup_pid));
    }
}
