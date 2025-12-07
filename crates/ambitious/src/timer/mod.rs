//! Timer module for scheduling delayed and repeated operations.
//!
//! This module provides Erlang-compatible timer functionality:
//!
//! - `send_after` - Send a message after a delay
//! - `send_interval` - Send a message repeatedly at intervals
//! - `apply_after` - Execute a function after a delay
//! - `apply_interval` - Execute a function repeatedly at intervals
//! - `exit_after` - Send an exit signal after a delay
//! - `kill_after` - Send a kill signal after a delay
//! - `cancel` - Cancel a scheduled timer
//! - `read` - Read remaining time without cancelling
//! - `sleep` - Sleep for a duration
//! - `tc` / `tc_async` - Measure execution time
//!
//! # Example
//!
//! ```ignore
//! use ambitious::timer;
//! use std::time::Duration;
//!
//! // Send a message after 1 second
//! let timer_ref = timer::send_after(Duration::from_secs(1), pid, &"hello")?;
//!
//! // Cancel if needed
//! if let Ok(remaining) = timer::cancel(timer_ref) {
//!     println!("Cancelled with {}ms remaining", remaining.as_millis());
//! }
//!
//! // Send a message every 500ms
//! let interval_ref = timer::send_interval(Duration::from_millis(500), pid, &"tick")?;
//!
//! // Execute a function after delay
//! timer::apply_after(Duration::from_secs(2), || {
//!     println!("Executed after 2 seconds!");
//! })?;
//!
//! // Measure execution time
//! let (elapsed, result) = timer::tc(|| expensive_computation());
//! ```

use crate::core::{ExitReason, Pid, Ref, Term};
use dashmap::DashMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::mpsc;

// =============================================================================
// Types
// =============================================================================

/// A reference to a scheduled timer, used for cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerRef(Ref);

impl TimerRef {
    /// Returns the underlying reference.
    pub fn inner(&self) -> Ref {
        self.0
    }
}

/// Error type for timer operations.
#[derive(Debug, Error)]
pub enum TimerError {
    /// The timer reference is invalid or already fired/cancelled.
    #[error("timer not found or already completed")]
    NotFound,
    /// Failed to send the cancellation signal.
    #[error("failed to cancel timer")]
    CancelFailed,
}

/// Result type for timer operations.
pub type TimerResult = Result<TimerRef, TimerError>;

// =============================================================================
// Internal Registry
// =============================================================================

/// Internal timer entry tracking.
struct TimerEntry {
    /// Channel to signal cancellation.
    cancel_tx: mpsc::Sender<()>,
    /// When the timer was started.
    started_at: Instant,
    /// Duration for one-shot timers, interval for repeating timers.
    duration: Duration,
}

/// Global timer registry.
static TIMERS: OnceLock<DashMap<Ref, TimerEntry>> = OnceLock::new();

/// Get or initialize the global timer registry.
fn timers() -> &'static DashMap<Ref, TimerEntry> {
    TIMERS.get_or_init(DashMap::new)
}

// =============================================================================
// Message Timers
// =============================================================================

/// Sends a message to a process after a delay.
///
/// Returns a [`TimerRef`] that can be used to cancel the timer.
/// The timer automatically cleans up if the target process dies.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// let timer_ref = timer::send_after(Duration::from_secs(5), pid, &"timeout")?;
/// ```
pub fn send_after<M: Term>(delay: Duration, pid: Pid, msg: &M) -> TimerResult {
    let timer_ref = TimerRef(Ref::new());
    let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
    let data = msg.encode();
    let ref_copy = timer_ref.0;

    timers().insert(
        ref_copy,
        TimerEntry {
            cancel_tx,
            started_at: Instant::now(),
            duration: delay,
        },
    );

    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = cancel_rx.recv() => {
                // Cancelled - do nothing
            }
            _ = tokio::time::sleep(delay) => {
                // Timer fired - send message
                let _ = crate::runtime::send_raw(pid, data);
            }
        }
        // Clean up registry
        timers().remove(&ref_copy);
    });

    Ok(timer_ref)
}

/// Sends a message to a process repeatedly at fixed intervals.
///
/// Returns a [`TimerRef`] that can be used to cancel the timer.
/// The timer automatically stops if the target process dies (send fails).
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// // Send "tick" every 100ms
/// let timer_ref = timer::send_interval(Duration::from_millis(100), pid, &"tick")?;
///
/// // Later, cancel it
/// timer::cancel(timer_ref)?;
/// ```
pub fn send_interval<M: Term>(interval: Duration, pid: Pid, msg: &M) -> TimerResult {
    let timer_ref = TimerRef(Ref::new());
    let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
    let data = msg.encode();
    let ref_copy = timer_ref.0;

    timers().insert(
        ref_copy,
        TimerEntry {
            cancel_tx,
            started_at: Instant::now(),
            duration: interval,
        },
    );

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick
        ticker.tick().await;

        loop {
            tokio::select! {
                biased;
                _ = cancel_rx.recv() => {
                    // Cancelled - clean up and exit
                    timers().remove(&ref_copy);
                    return;
                }
                _ = ticker.tick() => {
                    // Try to send; if it fails, target is dead
                    if crate::runtime::send_raw(pid, data.clone()).is_err() {
                        timers().remove(&ref_copy);
                        return;
                    }
                }
            }
        }
    });

    Ok(timer_ref)
}

// =============================================================================
// Function Timers
// =============================================================================

/// Executes a function after a delay.
///
/// The function is spawned in a new tokio task when the timer fires.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// timer::apply_after(Duration::from_secs(1), || {
///     println!("One second has passed!");
/// })?;
/// ```
pub fn apply_after<F>(delay: Duration, f: F) -> TimerResult
where
    F: FnOnce() + Send + 'static,
{
    let timer_ref = TimerRef(Ref::new());
    let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
    let ref_copy = timer_ref.0;

    timers().insert(
        ref_copy,
        TimerEntry {
            cancel_tx,
            started_at: Instant::now(),
            duration: delay,
        },
    );

    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = cancel_rx.recv() => {
                // Cancelled
            }
            _ = tokio::time::sleep(delay) => {
                // Timer fired - execute function
                f();
            }
        }
        timers().remove(&ref_copy);
    });

    Ok(timer_ref)
}

/// Executes a function repeatedly at fixed intervals.
///
/// The function is called in the timer's tokio task. Use this for
/// lightweight operations; for heavier work, consider spawning a
/// separate task inside the function.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
/// use std::sync::atomic::{AtomicU64, Ordering};
/// use std::sync::Arc;
///
/// let counter = Arc::new(AtomicU64::new(0));
/// let counter_clone = counter.clone();
///
/// timer::apply_interval(Duration::from_millis(100), move || {
///     counter_clone.fetch_add(1, Ordering::SeqCst);
/// })?;
/// ```
pub fn apply_interval<F>(interval: Duration, f: F) -> TimerResult
where
    F: Fn() + Send + Sync + 'static,
{
    let timer_ref = TimerRef(Ref::new());
    let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
    let ref_copy = timer_ref.0;

    timers().insert(
        ref_copy,
        TimerEntry {
            cancel_tx,
            started_at: Instant::now(),
            duration: interval,
        },
    );

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // Skip immediate first tick

        loop {
            tokio::select! {
                biased;
                _ = cancel_rx.recv() => {
                    timers().remove(&ref_copy);
                    return;
                }
                _ = ticker.tick() => {
                    f();
                }
            }
        }
    });

    Ok(timer_ref)
}

// =============================================================================
// Exit Timers
// =============================================================================

/// Sends an exit signal to a process after a delay.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use ambitious::ExitReason;
/// use std::time::Duration;
///
/// // Gracefully shutdown after 30 seconds
/// timer::exit_after(Duration::from_secs(30), pid, ExitReason::shutdown())?;
/// ```
pub fn exit_after(delay: Duration, pid: Pid, reason: ExitReason) -> TimerResult {
    let timer_ref = TimerRef(Ref::new());
    let (cancel_tx, mut cancel_rx) = mpsc::channel(1);
    let ref_copy = timer_ref.0;

    timers().insert(
        ref_copy,
        TimerEntry {
            cancel_tx,
            started_at: Instant::now(),
            duration: delay,
        },
    );

    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = cancel_rx.recv() => {
                // Cancelled
            }
            _ = tokio::time::sleep(delay) => {
                // Timer fired - send exit signal
                crate::runtime::exit(pid, reason);
            }
        }
        timers().remove(&ref_copy);
    });

    Ok(timer_ref)
}

/// Sends a kill signal to a process after a delay.
///
/// This is equivalent to `exit_after(delay, pid, ExitReason::Killed)`.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// // Force kill after 60 seconds
/// timer::kill_after(Duration::from_secs(60), pid)?;
/// ```
pub fn kill_after(delay: Duration, pid: Pid) -> TimerResult {
    exit_after(delay, pid, ExitReason::Killed)
}

// =============================================================================
// Control
// =============================================================================

/// Cancels a scheduled timer.
///
/// Returns the remaining time if the timer was still active, or an error
/// if the timer has already fired or was already cancelled.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// let timer_ref = timer::send_after(Duration::from_secs(10), pid, &"timeout")?;
///
/// // Cancel after 3 seconds
/// tokio::time::sleep(Duration::from_secs(3)).await;
/// match timer::cancel(timer_ref) {
///     Ok(remaining) => println!("Cancelled with {}s remaining", remaining.as_secs()),
///     Err(_) => println!("Timer already fired"),
/// }
/// ```
pub fn cancel(timer_ref: TimerRef) -> Result<Duration, TimerError> {
    if let Some((_, entry)) = timers().remove(&timer_ref.0) {
        let elapsed = entry.started_at.elapsed();
        let remaining = entry.duration.saturating_sub(elapsed);
        // Signal cancellation (ignore errors - task may have already exited)
        let _ = entry.cancel_tx.try_send(());
        Ok(remaining)
    } else {
        Err(TimerError::NotFound)
    }
}

/// Reads the remaining time on a timer without cancelling it.
///
/// Returns `Some(duration)` if the timer is still active, or `None` if
/// it has already fired or been cancelled.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// let timer_ref = timer::send_after(Duration::from_secs(10), pid, &"timeout")?;
///
/// if let Some(remaining) = timer::read(timer_ref) {
///     println!("Timer has {}s remaining", remaining.as_secs());
/// }
/// ```
pub fn read(timer_ref: TimerRef) -> Option<Duration> {
    timers().get(&timer_ref.0).map(|entry| {
        let elapsed = entry.started_at.elapsed();
        entry.duration.saturating_sub(elapsed)
    })
}

// =============================================================================
// Utilities
// =============================================================================

/// Suspends the current task for the specified duration.
///
/// This is a simple wrapper around `tokio::time::sleep` for convenience.
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
/// use std::time::Duration;
///
/// timer::sleep(Duration::from_millis(100)).await;
/// ```
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Measures the execution time of a synchronous function.
///
/// Returns a tuple of (elapsed_time, result).
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
///
/// let (elapsed, result) = timer::tc(|| {
///     expensive_computation()
/// });
/// println!("Computation took {:?}", elapsed);
/// ```
pub fn tc<F, R>(f: F) -> (Duration, R)
where
    F: FnOnce() -> R,
{
    let start = Instant::now();
    let result = f();
    (start.elapsed(), result)
}

/// Measures the execution time of an async function.
///
/// Returns a tuple of (elapsed_time, result).
///
/// # Example
///
/// ```ignore
/// use ambitious::timer;
///
/// let (elapsed, result) = timer::tc_async(|| async {
///     async_operation().await
/// }).await;
/// println!("Async operation took {:?}", elapsed);
/// ```
pub async fn tc_async<F, Fut, R>(f: F) -> (Duration, R)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = R>,
{
    let start = Instant::now();
    let result = f().await;
    (start.elapsed(), result)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    #[test]
    fn test_tc_measures_time() {
        let (elapsed, result) = tc(|| {
            std::thread::sleep(Duration::from_millis(50));
            42
        });
        assert_eq!(result, 42);
        assert!(elapsed >= Duration::from_millis(50));
        assert!(elapsed < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn test_tc_async_measures_time() {
        let (elapsed, result) = tc_async(|| async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            "done"
        })
        .await;
        assert_eq!(result, "done");
        assert!(elapsed >= Duration::from_millis(50));
        assert!(elapsed < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn test_sleep() {
        let start = Instant::now();
        sleep(Duration::from_millis(50)).await;
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn test_apply_after() {
        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();

        let timer_ref = apply_after(Duration::from_millis(50), move || {
            executed_clone.store(true, Ordering::SeqCst);
        })
        .unwrap();

        // Should not have executed yet
        assert!(!executed.load(Ordering::SeqCst));

        // Wait for timer to fire
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should have executed
        assert!(executed.load(Ordering::SeqCst));

        // Timer should be cleaned up
        assert!(read(timer_ref).is_none());
    }

    #[tokio::test]
    async fn test_apply_after_cancel() {
        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();

        let timer_ref = apply_after(Duration::from_millis(100), move || {
            executed_clone.store(true, Ordering::SeqCst);
        })
        .unwrap();

        // Cancel before it fires
        tokio::time::sleep(Duration::from_millis(30)).await;
        let remaining = cancel(timer_ref).unwrap();

        // Should have some time remaining
        assert!(remaining > Duration::from_millis(50));
        assert!(remaining <= Duration::from_millis(70));

        // Wait past when it would have fired
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should NOT have executed
        assert!(!executed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_apply_interval() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let timer_ref = apply_interval(Duration::from_millis(30), move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        // Wait for a few ticks
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should have executed multiple times
        let count = counter.load(Ordering::SeqCst);
        assert!(count >= 2, "Expected at least 2 executions, got {}", count);

        // Cancel
        cancel(timer_ref).unwrap();

        // Record current count
        let count_after_cancel = counter.load(Ordering::SeqCst);

        // Wait a bit more
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Count should not have increased (or at most by 1 due to race)
        let final_count = counter.load(Ordering::SeqCst);
        assert!(
            final_count <= count_after_cancel + 1,
            "Timer should have stopped"
        );
    }

    #[tokio::test]
    async fn test_read_timer() {
        let timer_ref = apply_after(Duration::from_millis(100), || {}).unwrap();

        // Should have time remaining
        let remaining = read(timer_ref).unwrap();
        assert!(remaining > Duration::from_millis(80));

        // Wait a bit
        tokio::time::sleep(Duration::from_millis(30)).await;

        // Should have less time remaining
        let remaining2 = read(timer_ref).unwrap();
        assert!(remaining2 < remaining);

        // Cancel and verify read returns None
        cancel(timer_ref).unwrap();
        assert!(read(timer_ref).is_none());
    }

    #[tokio::test]
    async fn test_cancel_already_fired() {
        let timer_ref = apply_after(Duration::from_millis(10), || {}).unwrap();

        // Wait for it to fire
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should return NotFound
        assert!(matches!(cancel(timer_ref), Err(TimerError::NotFound)));
    }
}
