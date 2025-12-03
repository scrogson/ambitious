//! Unique reference type.
//!
//! A [`Ref`] is a unique identifier used for monitors, timers, and other
//! one-time or cancellable operations.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for generating unique references.
static REF_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique reference.
///
/// References are used to identify monitors, timers, and other operations
/// that may need to be cancelled or matched against messages.
///
/// # Examples
///
/// ```
/// use starlang_core::Ref;
///
/// let r = Ref::new();
/// println!("Reference: {}", r);
///
/// // References are unique
/// let r2 = Ref::new();
/// assert_ne!(r, r2);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref(u64);

impl Ref {
    /// Creates a new unique reference.
    ///
    /// Each call to `new()` returns a globally unique `Ref`.
    ///
    /// # Examples
    ///
    /// ```
    /// use starlang_core::Ref;
    ///
    /// let r1 = Ref::new();
    /// let r2 = Ref::new();
    /// assert_ne!(r1, r2);
    /// ```
    pub fn new() -> Self {
        Self(REF_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Creates a `Ref` from a raw value.
    ///
    /// This is primarily used for deserialization and testing.
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw value of this reference.
    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

impl Default for Ref {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ref({})", self.0)
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ref_uniqueness() {
        let r1 = Ref::new();
        let r2 = Ref::new();
        assert_ne!(r1, r2);
    }

    #[test]
    fn test_ref_from_raw() {
        let r = Ref::from_raw(42);
        assert_eq!(r.as_raw(), 42);
    }

    #[test]
    fn test_ref_display() {
        let r = Ref::from_raw(123);
        assert_eq!(format!("{}", r), "#123");
        assert_eq!(format!("{:?}", r), "Ref(123)");
    }

    #[test]
    fn test_ref_serialization() {
        let r = Ref::from_raw(999);
        let bytes = postcard::to_allocvec(&r).unwrap();
        let decoded: Ref = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(r, decoded);
    }

    #[test]
    fn test_ref_hash() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        let r1 = Ref::new();
        let r2 = Ref::new();

        map.insert(r1, "first");
        map.insert(r2, "second");

        assert_eq!(map.get(&r1), Some(&"first"));
        assert_eq!(map.get(&r2), Some(&"second"));
    }
}
