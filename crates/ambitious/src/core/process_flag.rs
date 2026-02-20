//! Process flags for controlling process behavior.

/// Flags that control process behavior.
///
/// Used with `ambitious::flag()` to set process-level options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessFlag {
    /// When enabled, exit signals from linked processes are delivered
    /// as messages instead of terminating this process.
    TrapExit,
}
