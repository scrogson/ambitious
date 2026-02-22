//! ViaRegistry trait and Name enum for pluggable process registration.
//!
//! This module provides the building blocks for named process registration
//! at start time, mirroring Elixir's `name:` option on `GenServer.start_link/3`.
//!
//! # Name Variants
//!
//! - `Name::Local(s)` — register in the local process registry (same as `ambitious::register`)
//! - `Name::Global(s)` — register in the global distributed registry (future)
//! - `Name::Via(arc)` — register via a pluggable `ViaRegistry` backend
//!
//! # ViaRegistry Trait
//!
//! A **bound-name** pattern: each `Arc<dyn ViaRegistry>` captures both the
//! registry instance and the specific key. This avoids type-erased keys.
//!
//! ```ignore
//! let registry = Registry::new_unique("services");
//! let name = Name::via(registry.via("my_service", ()));
//! let pid = gen_server::start_link_named::<MyServer>(name, args).await?;
//! ```

use crate::core::Pid;
use std::sync::Arc;

/// Error returned when a name is already registered.
#[derive(Debug, Clone)]
pub struct AlreadyRegistered;

impl std::fmt::Display for AlreadyRegistered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "already registered")
    }
}

impl std::error::Error for AlreadyRegistered {}

/// A pluggable registry backend for process registration.
///
/// Each `Arc<dyn ViaRegistry>` captures both the registry instance and the
/// specific key (bound-name pattern), so the trait methods take no key argument.
pub trait ViaRegistry: Send + Sync + 'static {
    /// Register `pid` under this name. Returns `Err` if already taken.
    fn register_name(&self, pid: Pid) -> Result<(), AlreadyRegistered>;

    /// Unregister this name.
    fn unregister_name(&self);

    /// Look up the pid for this name.
    fn whereis_name(&self) -> Option<Pid>;
}

/// How a process should be registered at start time.
///
/// This is passed to `start_named` / `start_link_named` functions.
/// It excludes `Pid` (which doesn't make sense for registration).
#[derive(Clone)]
pub enum Name {
    /// Register in the local process registry.
    Local(String),
    /// Register in the global distributed registry (future).
    Global(String),
    /// Register via a pluggable `ViaRegistry` backend.
    Via(Arc<dyn ViaRegistry>),
}

impl Name {
    /// Create a local name.
    pub fn local(name: impl Into<String>) -> Self {
        Name::Local(name.into())
    }

    /// Create a global name.
    pub fn global(name: impl Into<String>) -> Self {
        Name::Global(name.into())
    }

    /// Create a via name from a pluggable registry.
    pub fn via(registry: Arc<dyn ViaRegistry>) -> Self {
        Name::Via(registry)
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Name::Local(s) => write!(f, "Name::Local({:?})", s),
            Name::Global(s) => write!(f, "Name::Global({:?})", s),
            Name::Via(_) => write!(f, "Name::Via(...)"),
        }
    }
}

impl From<Name> for super::ServerRef {
    fn from(name: Name) -> Self {
        match name {
            Name::Local(s) => super::ServerRef::Name(s),
            Name::Global(s) => super::ServerRef::Name(s),
            Name::Via(via) => super::ServerRef::Via(via),
        }
    }
}

/// Register a process under the given name.
///
/// Returns `Err(AlreadyRegistered)` if the name is already taken.
pub(crate) fn register_name(name: &Name, pid: Pid) -> Result<(), AlreadyRegistered> {
    match name {
        Name::Local(s) => {
            // Check if already registered
            if crate::whereis(s).is_some() {
                return Err(AlreadyRegistered);
            }
            crate::register(s.clone(), pid);
            Ok(())
        }
        Name::Global(s) => {
            // For now, global behaves the same as local
            if crate::whereis(s).is_some() {
                return Err(AlreadyRegistered);
            }
            crate::register(s.clone(), pid);
            Ok(())
        }
        Name::Via(via) => via.register_name(pid),
    }
}

/// Unregister a process from the given name.
pub(crate) fn unregister_name(name: &Name) {
    match name {
        Name::Local(s) => {
            crate::unregister(s);
        }
        Name::Global(s) => {
            crate::unregister(s);
        }
        Name::Via(via) => {
            via.unregister_name();
        }
    }
}
