//! Process exit reasons.
//!
//! An [`ExitReason`] describes why a process terminated. This is used in
//! exit signals, monitor DOWN messages, and supervisor restart decisions.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The reason a process exited.
///
/// Exit reasons determine how linked processes and supervisors respond to
/// process termination.
///
/// # Normal vs Abnormal Exits
///
/// - **Normal exits** ([`ExitReason::Normal`], [`ExitReason::Shutdown`],
///   [`ExitReason::ShutdownReason`]): Do not trigger restarts for `Transient`
///   children in supervisors. Links with `trap_exit = false` do not propagate.
///
/// - **Abnormal exits** ([`ExitReason::Killed`], [`ExitReason::Error`]):
///   Trigger restarts and propagate through links.
///
/// # Examples
///
/// ```
/// use starlang_core::ExitReason;
///
/// let reason = ExitReason::Normal;
/// assert!(reason.is_normal());
///
/// let reason = ExitReason::Error("connection lost".to_string());
/// assert!(!reason.is_normal());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExitReason {
    /// Process completed successfully.
    ///
    /// This is the standard exit reason when a process finishes its work
    /// without errors.
    #[default]
    Normal,

    /// Process was asked to shut down.
    ///
    /// Used when a supervisor or other controller requests graceful termination.
    Shutdown,

    /// Process was asked to shut down with a specific reason.
    ///
    /// Similar to `Shutdown` but includes additional context.
    ShutdownReason(String),

    /// Process was forcefully terminated.
    ///
    /// Used with `Process::exit(pid, ExitReason::Killed)` for unconditional
    /// termination that cannot be trapped.
    Killed,

    /// Process terminated due to an error.
    ///
    /// This is an abnormal exit that will trigger supervisor restarts and
    /// propagate through links.
    Error(String),
}

impl ExitReason {
    /// Returns `true` if this is a normal exit reason.
    ///
    /// Normal exits include `Normal`, `Shutdown`, and `ShutdownReason`.
    /// These do not trigger supervisor restarts for `Transient` children.
    ///
    /// # Examples
    ///
    /// ```
    /// use starlang_core::ExitReason;
    ///
    /// assert!(ExitReason::Normal.is_normal());
    /// assert!(ExitReason::Shutdown.is_normal());
    /// assert!(ExitReason::ShutdownReason("done".into()).is_normal());
    /// assert!(!ExitReason::Killed.is_normal());
    /// assert!(!ExitReason::Error("oops".into()).is_normal());
    /// ```
    pub fn is_normal(&self) -> bool {
        matches!(
            self,
            ExitReason::Normal | ExitReason::Shutdown | ExitReason::ShutdownReason(_)
        )
    }

    /// Returns `true` if this is an abnormal exit reason.
    ///
    /// Abnormal exits include `Killed` and `Error`. These trigger supervisor
    /// restarts and propagate through links to non-trapping processes.
    #[inline]
    pub fn is_abnormal(&self) -> bool {
        !self.is_normal()
    }

    /// Returns `true` if this is the `Killed` variant.
    ///
    /// The `Killed` reason is special: it cannot be trapped and always
    /// terminates the target process.
    #[inline]
    pub fn is_killed(&self) -> bool {
        matches!(self, ExitReason::Killed)
    }

    /// Creates an error exit reason from any displayable type.
    ///
    /// # Examples
    ///
    /// ```
    /// use starlang_core::ExitReason;
    ///
    /// let reason = ExitReason::error("something went wrong");
    /// assert_eq!(reason, ExitReason::Error("something went wrong".to_string()));
    /// ```
    pub fn error(msg: impl fmt::Display) -> Self {
        ExitReason::Error(msg.to_string())
    }

    /// Creates a shutdown exit reason with a message.
    ///
    /// # Examples
    ///
    /// ```
    /// use starlang_core::ExitReason;
    ///
    /// let reason = ExitReason::shutdown("maintenance");
    /// assert!(reason.is_normal());
    /// ```
    pub fn shutdown(msg: impl fmt::Display) -> Self {
        ExitReason::ShutdownReason(msg.to_string())
    }
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExitReason::Normal => write!(f, "normal"),
            ExitReason::Shutdown => write!(f, "shutdown"),
            ExitReason::ShutdownReason(reason) => write!(f, "shutdown: {}", reason),
            ExitReason::Killed => write!(f, "killed"),
            ExitReason::Error(msg) => write!(f, "error: {}", msg),
        }
    }
}

impl From<&str> for ExitReason {
    fn from(s: &str) -> Self {
        ExitReason::Error(s.to_string())
    }
}

impl From<String> for ExitReason {
    fn from(s: String) -> Self {
        ExitReason::Error(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_normal() {
        assert!(ExitReason::Normal.is_normal());
        assert!(ExitReason::Shutdown.is_normal());
        assert!(ExitReason::ShutdownReason("test".into()).is_normal());
        assert!(!ExitReason::Killed.is_normal());
        assert!(!ExitReason::Error("test".into()).is_normal());
    }

    #[test]
    fn test_is_abnormal() {
        assert!(!ExitReason::Normal.is_abnormal());
        assert!(ExitReason::Killed.is_abnormal());
        assert!(ExitReason::Error("test".into()).is_abnormal());
    }

    #[test]
    fn test_is_killed() {
        assert!(ExitReason::Killed.is_killed());
        assert!(!ExitReason::Normal.is_killed());
        assert!(!ExitReason::Error("test".into()).is_killed());
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", ExitReason::Normal), "normal");
        assert_eq!(format!("{}", ExitReason::Shutdown), "shutdown");
        assert_eq!(
            format!("{}", ExitReason::ShutdownReason("timeout".into())),
            "shutdown: timeout"
        );
        assert_eq!(format!("{}", ExitReason::Killed), "killed");
        assert_eq!(
            format!("{}", ExitReason::Error("oops".into())),
            "error: oops"
        );
    }

    #[test]
    fn test_from_str() {
        let reason: ExitReason = "something failed".into();
        assert_eq!(reason, ExitReason::Error("something failed".to_string()));
    }

    #[test]
    fn test_serialization() {
        let reasons = vec![
            ExitReason::Normal,
            ExitReason::Shutdown,
            ExitReason::ShutdownReason("test".into()),
            ExitReason::Killed,
            ExitReason::Error("error".into()),
        ];

        for reason in reasons {
            let bytes = postcard::to_allocvec(&reason).unwrap();
            let decoded: ExitReason = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(reason, decoded);
        }
    }

    #[test]
    fn test_helper_constructors() {
        let err = ExitReason::error("failed");
        assert_eq!(err, ExitReason::Error("failed".to_string()));

        let shut = ExitReason::shutdown("maintenance");
        assert_eq!(shut, ExitReason::ShutdownReason("maintenance".to_string()));
    }
}
