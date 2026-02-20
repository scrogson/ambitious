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
/// See the `dynamic_supervisor` module for details.
pub mod dynamic_supervisor;

pub use core::{
    Supervisor, SupervisorInit, count_children, delete_child, restart_child, start, start_link,
    terminate_child, which_children,
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

    #[tokio::test]
    async fn test_supervisor_shutdown_waits_for_child() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        // Track that child was started
        let child_started = Arc::new(AtomicBool::new(false));
        let child_started_clone = child_started.clone();

        struct WaitSupervisor;

        impl Supervisor for WaitSupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicBool>);

            fn init((handle, started_flag): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne),
                    vec![
                        ChildSpec::new("worker", move || {
                            let h = handle_clone.clone();
                            let sf = started_flag.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    sf.store(true, Ordering::SeqCst);
                                    // Simple worker that waits
                                    while let Ok(Some(_)) =
                                        crate::recv_timeout(Duration::from_secs(60)).await
                                    {
                                    }
                                });
                                Ok(pid)
                            }
                        })
                        // Use Timeout shutdown - supervisor should wait for child
                        .shutdown(ShutdownType::Timeout(Duration::from_millis(500))),
                    ],
                )
            }
        }

        let sup_pid = start::<WaitSupervisor>(&handle, (handle.clone(), child_started_clone))
            .await
            .unwrap();

        // Give it time to start children
        sleep(Duration::from_millis(50)).await;
        assert!(handle.alive(sup_pid));
        assert!(
            child_started.load(Ordering::SeqCst),
            "Child should have started"
        );

        // Exit supervisor - it should wait for child
        let _ = handle.exit(sup_pid, crate::core::ExitReason::Shutdown);

        // Wait for shutdown
        sleep(Duration::from_millis(100)).await;

        // Supervisor should be dead now
        assert!(!handle.alive(sup_pid), "Supervisor should have terminated");
    }

    #[tokio::test]
    async fn test_supervisor_brutal_kill_immediate() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let child_started = Arc::new(AtomicBool::new(false));
        let child_started_clone = child_started.clone();

        struct BrutalSupervisor;

        impl Supervisor for BrutalSupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicBool>);

            fn init((handle, started_flag): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne),
                    vec![
                        ChildSpec::new("brutal_worker", move || {
                            let h = handle_clone.clone();
                            let sf = started_flag.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    sf.store(true, Ordering::SeqCst);
                                    // Worker that waits
                                    while let Ok(Some(_)) =
                                        crate::recv_timeout(Duration::from_secs(60)).await
                                    {
                                    }
                                });
                                Ok(pid)
                            }
                        })
                        // Use BrutalKill - supervisor should kill immediately
                        .shutdown(ShutdownType::BrutalKill),
                    ],
                )
            }
        }

        let sup_pid = start::<BrutalSupervisor>(&handle, (handle.clone(), child_started_clone))
            .await
            .unwrap();

        // Give it time to start children
        sleep(Duration::from_millis(50)).await;
        assert!(handle.alive(sup_pid));
        assert!(
            child_started.load(Ordering::SeqCst),
            "Child should have started"
        );

        let before = std::time::Instant::now();

        // Exit supervisor
        let _ = handle.exit(sup_pid, crate::core::ExitReason::Shutdown);

        // Wait for shutdown
        sleep(Duration::from_millis(50)).await;

        let elapsed = before.elapsed();

        // BrutalKill should be very fast (no waiting)
        assert!(
            elapsed < Duration::from_millis(100),
            "BrutalKill should terminate quickly, took {:?}",
            elapsed
        );

        assert!(!handle.alive(sup_pid), "Supervisor should have terminated");
    }

    #[tokio::test]
    async fn test_supervisor_exits_with_error_on_max_restarts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        // Track restart count
        let restart_count = Arc::new(AtomicUsize::new(0));
        let restart_count_clone = restart_count.clone();

        struct CrashySupervisor;

        impl Supervisor for CrashySupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicUsize>);

            fn init((handle, count): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(2) // Allow only 2 restarts
                        .max_seconds(10),
                    vec![
                        ChildSpec::new("crashy", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    c.fetch_add(1, Ordering::SeqCst);
                                    // Small delay so supervisor can start
                                    sleep(Duration::from_millis(20)).await;
                                    // Crash with an error
                                    crate::runtime::set_exit_reason(ExitReason::error(
                                        "intentional crash",
                                    ));
                                });
                                Ok(pid)
                            }
                        })
                        .restart(RestartType::Permanent),
                    ],
                )
            }
        }

        let sup_pid = start::<CrashySupervisor>(&handle, (handle.clone(), restart_count_clone))
            .await
            .unwrap();

        // Give it time to hit max restarts
        sleep(Duration::from_millis(500)).await;

        // Supervisor should be dead (exited due to max restart intensity)
        assert!(
            !handle.alive(sup_pid),
            "Supervisor should have terminated after max restarts"
        );

        // Should have attempted restarts (initial + 2 restarts = 3 starts)
        let count = restart_count.load(Ordering::SeqCst);
        assert!(
            count >= 3,
            "Child should have been started at least 3 times, got {}",
            count
        );
    }

    #[tokio::test]
    async fn test_failure_escalation_to_parent_supervisor() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        // Track child supervisor crash and parent exit
        let child_sup_crashed = Arc::new(AtomicBool::new(false));
        let parent_received_exit = Arc::new(AtomicBool::new(false));
        let child_restart_count = Arc::new(AtomicUsize::new(0));

        let child_sup_crashed_clone = child_sup_crashed.clone();
        let _parent_received_exit_clone = parent_received_exit.clone();
        let child_restart_count_clone = child_restart_count.clone();

        // Child supervisor that will hit max restarts
        struct ChildSupervisor;

        impl Supervisor for ChildSupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicUsize>);

            fn init((handle, count): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(1) // Only 1 restart allowed
                        .max_seconds(10),
                    vec![
                        ChildSpec::new("crashy_worker", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    c.fetch_add(1, Ordering::SeqCst);
                                    // Crash immediately
                                    crate::runtime::set_exit_reason(ExitReason::error(
                                        "worker crash",
                                    ));
                                });
                                Ok(pid)
                            }
                        })
                        .restart(RestartType::Permanent),
                    ],
                )
            }
        }

        // Parent supervisor that supervises the child supervisor
        struct ParentSupervisor;

        impl Supervisor for ParentSupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicUsize>, Arc<AtomicBool>);

            fn init((handle, count, crashed_flag): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(0) // Don't restart child supervisors
                        .max_seconds(10),
                    vec![
                        ChildSpec::new("child_supervisor", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            let cf = crashed_flag.clone();
                            async move {
                                let parent_pid = crate::runtime::current_pid();
                                let pid =
                                    start_link::<ChildSupervisor>(&h, parent_pid, (h.clone(), c))
                                        .await
                                        .map_err(|e| StartChildError::Failed(format!("{:?}", e)))?;
                                // Mark that we started
                                cf.store(false, Ordering::SeqCst);
                                Ok(pid)
                            }
                        })
                        .supervisor()
                        .restart(RestartType::Permanent),
                    ],
                )
            }
        }

        let parent_pid = start::<ParentSupervisor>(
            &handle,
            (
                handle.clone(),
                child_restart_count_clone,
                child_sup_crashed_clone,
            ),
        )
        .await
        .unwrap();

        // Give it time for the child supervisor to crash and propagate
        sleep(Duration::from_millis(500)).await;

        // The parent should also be dead because it couldn't restart the child
        // (max_restarts = 0)
        assert!(
            !handle.alive(parent_pid),
            "Parent supervisor should have terminated after child supervisor failed"
        );

        // Child should have crashed at least twice (initial + 1 restart)
        let count = child_restart_count.load(Ordering::SeqCst);
        assert!(
            count >= 2,
            "Child worker should have been started at least 2 times, got {}",
            count
        );
    }

    #[tokio::test]
    async fn test_transient_child_normal_exit_no_restart() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let start_count = Arc::new(AtomicUsize::new(0));
        let start_count_clone = start_count.clone();

        struct TransientSupervisor;

        impl Supervisor for TransientSupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicUsize>);

            fn init((handle, count): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(10)
                        .max_seconds(10),
                    vec![
                        ChildSpec::new("transient_worker", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    c.fetch_add(1, Ordering::SeqCst);
                                    // Exit normally - transient should NOT restart
                                });
                                Ok(pid)
                            }
                        })
                        .restart(RestartType::Transient),
                    ],
                )
            }
        }

        let sup_pid = start::<TransientSupervisor>(&handle, (handle.clone(), start_count_clone))
            .await
            .unwrap();

        // Give it time to process
        sleep(Duration::from_millis(200)).await;

        // Supervisor should still be alive
        assert!(handle.alive(sup_pid), "Supervisor should still be running");

        // Child should only have started once (no restart on normal exit)
        let count = start_count.load(Ordering::SeqCst);
        assert_eq!(
            count, 1,
            "Transient child should start only once on normal exit, got {}",
            count
        );
    }

    #[tokio::test]
    async fn test_transient_child_abnormal_exit_restarts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let start_count = Arc::new(AtomicUsize::new(0));
        let start_count_clone = start_count.clone();

        struct TransientCrashSupervisor;

        impl Supervisor for TransientCrashSupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicUsize>);

            fn init((handle, count): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(2)
                        .max_seconds(10),
                    vec![
                        ChildSpec::new("transient_crashy", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    c.fetch_add(1, Ordering::SeqCst);
                                    // Small delay so supervisor can start
                                    sleep(Duration::from_millis(20)).await;
                                    // Exit abnormally - transient SHOULD restart
                                    crate::runtime::set_exit_reason(ExitReason::error("crash"));
                                });
                                Ok(pid)
                            }
                        })
                        .restart(RestartType::Transient),
                    ],
                )
            }
        }

        let sup_pid =
            start::<TransientCrashSupervisor>(&handle, (handle.clone(), start_count_clone))
                .await
                .unwrap();

        // Give it time to hit max restarts
        sleep(Duration::from_millis(500)).await;

        // Supervisor should be dead (max restart intensity reached)
        assert!(
            !handle.alive(sup_pid),
            "Supervisor should have terminated after max restarts"
        );

        // Child should have started multiple times (initial + restarts)
        let count = start_count.load(Ordering::SeqCst);
        assert!(
            count >= 3,
            "Transient child should restart on abnormal exit, got {} starts",
            count
        );
    }

    #[tokio::test]
    async fn test_temporary_child_never_restarts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let start_count = Arc::new(AtomicUsize::new(0));
        let start_count_clone = start_count.clone();

        struct TemporarySupervisor;

        impl Supervisor for TemporarySupervisor {
            type InitArg = (RuntimeHandle, Arc<AtomicUsize>);

            fn init((handle, count): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(10)
                        .max_seconds(10),
                    vec![
                        ChildSpec::new("temporary_crashy", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    c.fetch_add(1, Ordering::SeqCst);
                                    // Exit abnormally - but temporary should NOT restart
                                    crate::runtime::set_exit_reason(ExitReason::error("crash"));
                                });
                                Ok(pid)
                            }
                        })
                        .restart(RestartType::Temporary),
                    ],
                )
            }
        }

        let sup_pid = start::<TemporarySupervisor>(&handle, (handle.clone(), start_count_clone))
            .await
            .unwrap();

        // Give it time to process
        sleep(Duration::from_millis(200)).await;

        // Supervisor should still be alive
        assert!(
            handle.alive(sup_pid),
            "Supervisor should still be running with temporary child"
        );

        // Child should only have started once (no restart for temporary)
        let count = start_count.load(Ordering::SeqCst);
        assert_eq!(
            count, 1,
            "Temporary child should never restart, got {} starts",
            count
        );
    }

    // =========================================================================
    // Child management command tests
    // =========================================================================

    /// Helper: creates a supervisor with a single long-lived worker child.
    /// Returns the supervisor PID. Uses the provided handle for spawning.
    async fn setup_supervisor_with_worker(
        handle: &RuntimeHandle,
        start_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Pid {
        use std::sync::atomic::Ordering;
        struct WorkerSup;

        impl Supervisor for WorkerSup {
            type InitArg = (
                RuntimeHandle,
                std::sync::Arc<std::sync::atomic::AtomicUsize>,
            );

            fn init((handle, count): Self::InitArg) -> SupervisorInit {
                let handle_clone = handle.clone();
                SupervisorInit::new(
                    SupervisorFlags::new(Strategy::OneForOne)
                        .max_restarts(10)
                        .max_seconds(60),
                    vec![
                        ChildSpec::new("worker", move || {
                            let h = handle_clone.clone();
                            let c = count.clone();
                            async move {
                                let pid = h.spawn(move || async move {
                                    c.fetch_add(1, Ordering::SeqCst);
                                    // Long-lived worker that waits for messages
                                    while let Ok(Some(_)) =
                                        crate::runtime::recv_timeout(Duration::from_secs(60)).await
                                    {
                                    }
                                });
                                Ok(pid)
                            }
                        })
                        .restart(RestartType::Permanent)
                        .shutdown(ShutdownType::BrutalKill),
                    ],
                )
            }
        }

        start::<WorkerSup>(handle, (handle.clone(), start_count))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_terminate_child() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let start_count = Arc::new(AtomicUsize::new(0));
        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();
        let start_count_clone = start_count.clone();
        let handle_clone = handle.clone();

        handle.spawn(move || async move {
            let h = handle_clone;
            let sup_pid = setup_supervisor_with_worker(&h, start_count_clone).await;

            // Give child time to start
            sleep(Duration::from_millis(50)).await;

            // 1. Terminate running child - should succeed
            let result = terminate_child(&h, sup_pid, "worker").await;
            assert!(
                result.is_ok(),
                "terminate_child should succeed: {:?}",
                result
            );

            // Give time for state to sync
            sleep(Duration::from_millis(50)).await;

            // Verify child is stopped via count_children
            let counts = count_children(&h, sup_pid).await.unwrap();
            assert_eq!(counts.active, 0, "child should be stopped after terminate");
            assert_eq!(counts.specs, 1, "spec should still exist");

            // 2. Terminate already stopped child - should be idempotent (succeed)
            let result2 = terminate_child(&h, sup_pid, "worker").await;
            assert!(
                result2.is_ok(),
                "terminate_child on stopped child should succeed: {:?}",
                result2
            );

            // 3. Terminate nonexistent child - should error
            let result3 = terminate_child(&h, sup_pid, "nonexistent").await;
            assert!(
                result3.is_err(),
                "terminate_child on nonexistent should error"
            );

            test_passed_clone.store(true, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(500)).await;
        assert!(
            test_passed.load(Ordering::SeqCst),
            "test_terminate_child did not complete"
        );
    }

    #[tokio::test]
    async fn test_delete_child() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let start_count = Arc::new(AtomicUsize::new(0));
        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();
        let start_count_clone = start_count.clone();
        let handle_clone = handle.clone();

        handle.spawn(move || async move {
            let h = handle_clone;
            let sup_pid = setup_supervisor_with_worker(&h, start_count_clone).await;

            // Give child time to start
            sleep(Duration::from_millis(50)).await;

            // 1. Delete running child - should error with Running
            let result = delete_child(&h, sup_pid, "worker").await;
            assert!(
                result.is_err(),
                "delete_child on running child should error"
            );
            if let Err(ref e) = result {
                assert!(
                    matches!(e, DeleteError::Running(_)),
                    "expected Running error, got: {:?}",
                    e
                );
            }

            // 2. Terminate first, then delete - should succeed
            let _ = terminate_child(&h, sup_pid, "worker").await;
            sleep(Duration::from_millis(50)).await;

            let result2 = delete_child(&h, sup_pid, "worker").await;
            assert!(
                result2.is_ok(),
                "delete_child after terminate should succeed: {:?}",
                result2
            );

            // Verify spec is gone
            sleep(Duration::from_millis(50)).await;
            let counts = count_children(&h, sup_pid).await.unwrap();
            assert_eq!(counts.specs, 0, "spec should be removed after delete");

            // 3. Delete again - should error with NotFound
            let result3 = delete_child(&h, sup_pid, "worker").await;
            assert!(
                result3.is_err(),
                "delete_child on deleted child should error"
            );
            if let Err(ref e) = result3 {
                assert!(
                    matches!(e, DeleteError::NotFound(_)),
                    "expected NotFound error, got: {:?}",
                    e
                );
            }

            test_passed_clone.store(true, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(500)).await;
        assert!(
            test_passed.load(Ordering::SeqCst),
            "test_delete_child did not complete"
        );
    }

    #[tokio::test]
    async fn test_restart_child() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let runtime = Runtime::new();
        let handle = runtime.handle();

        let start_count = Arc::new(AtomicUsize::new(0));
        let test_passed = Arc::new(AtomicBool::new(false));
        let test_passed_clone = test_passed.clone();
        let start_count_clone = start_count.clone();
        let handle_clone = handle.clone();

        handle.spawn(move || async move {
            let h = handle_clone;
            let sup_pid = setup_supervisor_with_worker(&h, start_count_clone.clone()).await;

            // Give child time to start
            sleep(Duration::from_millis(50)).await;

            // Initial start count should be 1
            let initial_count = start_count_clone.load(Ordering::SeqCst);
            assert_eq!(initial_count, 1, "child should have started once initially");

            // 1. Restart running child - should error with AlreadyRunning
            let result = restart_child(&h, sup_pid, "worker").await;
            assert!(
                result.is_err(),
                "restart_child on running child should error"
            );
            if let Err(ref e) = result {
                assert!(
                    matches!(e, RestartError::AlreadyRunning(_)),
                    "expected AlreadyRunning error, got: {:?}",
                    e
                );
            }

            // 2. Terminate, then restart - should succeed
            let _ = terminate_child(&h, sup_pid, "worker").await;
            sleep(Duration::from_millis(50)).await;

            let result2 = restart_child(&h, sup_pid, "worker").await;
            assert!(
                result2.is_ok(),
                "restart_child after terminate should succeed: {:?}",
                result2
            );
            let new_pid = result2.unwrap();
            assert!(h.alive(new_pid), "restarted child should be alive");

            // Give child time to increment counter
            sleep(Duration::from_millis(50)).await;

            // 3. Verify start count incremented
            let final_count = start_count_clone.load(Ordering::SeqCst);
            assert_eq!(
                final_count, 2,
                "start count should be 2 after restart, got {}",
                final_count
            );

            // Verify child is active via count_children
            let counts = count_children(&h, sup_pid).await.unwrap();
            assert_eq!(counts.active, 1, "child should be active after restart");

            test_passed_clone.store(true, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(500)).await;
        assert!(
            test_passed.load(Ordering::SeqCst),
            "test_restart_child did not complete"
        );
    }
}
