//! Atom (interned string) implementation for Starlang.
//!
//! Atoms are immutable, interned strings that provide:
//! - O(1) equality comparison (just compare indices)
//! - Cheap cloning (Copy trait, just a u32)
//! - Thread-safe global atom table
//!
//! # Example
//!
//! ```
//! use starlang_atom::{Atom, atom};
//!
//! let a1 = atom!("hello");
//! let a2 = atom!("hello");
//! let a3 = atom!("world");
//!
//! assert_eq!(a1, a2);  // Same string = same atom
//! assert_ne!(a1, a3);  // Different string = different atom
//! assert_eq!(a1.as_str(), "hello");
//! ```

use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::sync::OnceLock;

/// An interned string.
///
/// Atoms are cheap to clone (just a u32 index) and cheap to compare
/// (just compare the indices). The actual string data is stored in
/// a global table.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Atom(u32);

/// Global atom table.
static ATOM_TABLE: OnceLock<AtomTable> = OnceLock::new();

/// The atom table stores all interned strings.
struct AtomTable {
    /// Map from string to index.
    string_to_index: DashMap<String, u32>,
    /// Map from index to string (for reverse lookup).
    index_to_string: RwLock<Vec<String>>,
}

impl AtomTable {
    fn new() -> Self {
        Self {
            string_to_index: DashMap::new(),
            index_to_string: RwLock::new(Vec::new()),
        }
    }

    /// Intern a string, returning its atom.
    fn intern(&self, s: &str) -> Atom {
        // Fast path: already interned
        if let Some(index) = self.string_to_index.get(s) {
            return Atom(*index);
        }

        // Slow path: need to add it
        let mut strings = self.index_to_string.write();

        // Double-check after acquiring write lock
        if let Some(index) = self.string_to_index.get(s) {
            return Atom(*index);
        }

        let index = strings.len() as u32;
        strings.push(s.to_string());
        self.string_to_index.insert(s.to_string(), index);

        Atom(index)
    }

    /// Get the string for an atom.
    fn get(&self, atom: Atom) -> Option<String> {
        let strings = self.index_to_string.read();
        strings.get(atom.0 as usize).cloned()
    }
}

/// Get the global atom table, initializing if necessary.
fn table() -> &'static AtomTable {
    ATOM_TABLE.get_or_init(AtomTable::new)
}

impl Atom {
    /// Create an atom from a string.
    ///
    /// If the string has been interned before, returns the existing atom.
    /// Otherwise, adds it to the atom table.
    pub fn from_str(s: &str) -> Self {
        table().intern(s)
    }

    /// Get the string value of this atom.
    ///
    /// # Panics
    ///
    /// Panics if the atom is invalid (should never happen in normal use).
    pub fn as_str(&self) -> String {
        table()
            .get(*self)
            .expect("invalid atom index")
    }

    /// Get the internal index of this atom.
    ///
    /// This is mainly useful for debugging.
    pub fn index(&self) -> u32 {
        self.0
    }
}

impl From<&str> for Atom {
    fn from(s: &str) -> Self {
        Atom::from_str(s)
    }
}

impl From<String> for Atom {
    fn from(s: String) -> Self {
        Atom::from_str(&s)
    }
}

impl From<&String> for Atom {
    fn from(s: &String) -> Self {
        Atom::from_str(s)
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Atom({:?})", self.as_str())
    }
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// Serialize as the string value
impl Serialize for Atom {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_str().serialize(serializer)
    }
}

// Deserialize by interning the string
impl<'de> Deserialize<'de> for Atom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Atom::from_str(&s))
    }
}

/// Create an atom from a string literal.
///
/// # Example
///
/// ```
/// use starlang_atom::atom;
///
/// let node = atom!("node1@localhost");
/// ```
#[macro_export]
macro_rules! atom {
    ($s:expr) => {
        $crate::Atom::from_str($s)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atom_equality() {
        let a1 = atom!("hello");
        let a2 = atom!("hello");
        let a3 = atom!("world");

        assert_eq!(a1, a2);
        assert_ne!(a1, a3);
    }

    #[test]
    fn test_atom_as_str() {
        let a = atom!("test_string");
        assert_eq!(a.as_str(), "test_string");
    }

    #[test]
    fn test_atom_from_string() {
        let s = String::from("dynamic");
        let a1 = Atom::from(&s);
        let a2 = atom!("dynamic");
        assert_eq!(a1, a2);
    }

    #[test]
    fn test_atom_display() {
        let a = atom!("display_test");
        assert_eq!(format!("{}", a), "display_test");
    }

    #[test]
    fn test_atom_debug() {
        let a = atom!("debug_test");
        assert_eq!(format!("{:?}", a), "Atom(\"debug_test\")");
    }

    #[test]
    fn test_atom_copy() {
        let a1 = atom!("copy_test");
        let a2 = a1; // Copy
        assert_eq!(a1, a2);
    }

    #[test]
    fn test_atom_serialize_deserialize() {
        let original = atom!("serialize_test");
        let serialized = postcard::to_allocvec(&original).unwrap();
        let deserialized: Atom = postcard::from_bytes(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }
}
