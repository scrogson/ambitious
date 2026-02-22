//! ServerRef - unified way to address a GenServer.

use crate::core::Pid;
use std::sync::Arc;

use super::via::ViaRegistry;

/// A reference to a GenServer, either by PID, registered name, or via a
/// pluggable registry.
///
/// This allows GenServer client functions (`call`, `cast`, `stop`) to
/// accept either a `Pid`, a name string, or a `Via` registry reference.
///
/// # Examples
///
/// ```ignore
/// use ambitious::gen_server::{call, ServerRef};
///
/// // By PID (most common)
/// let reply = call::<MyServer, _>(pid, msg, timeout).await?;
///
/// // By registered name
/// let reply = call::<MyServer, _>("my_server", msg, timeout).await?;
/// ```
pub enum ServerRef {
    /// Reference by process ID.
    Pid(Pid),
    /// Reference by registered name.
    Name(String),
    /// Reference via a pluggable registry.
    Via(Arc<dyn ViaRegistry>),
}

impl ServerRef {
    /// Resolves the ServerRef to a Pid.
    ///
    /// Returns `None` if the name is not registered.
    pub fn resolve(&self) -> Option<Pid> {
        match self {
            ServerRef::Pid(pid) => Some(*pid),
            ServerRef::Name(name) => crate::whereis(name),
            ServerRef::Via(via) => via.whereis_name(),
        }
    }
}

impl Clone for ServerRef {
    fn clone(&self) -> Self {
        match self {
            ServerRef::Pid(pid) => ServerRef::Pid(*pid),
            ServerRef::Name(name) => ServerRef::Name(name.clone()),
            ServerRef::Via(via) => ServerRef::Via(Arc::clone(via)),
        }
    }
}

impl std::fmt::Debug for ServerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerRef::Pid(pid) => f.debug_tuple("Pid").field(pid).finish(),
            ServerRef::Name(name) => f.debug_tuple("Name").field(name).finish(),
            ServerRef::Via(_) => f.debug_tuple("Via").field(&"...").finish(),
        }
    }
}

impl From<Pid> for ServerRef {
    fn from(pid: Pid) -> Self {
        ServerRef::Pid(pid)
    }
}

impl From<&str> for ServerRef {
    fn from(name: &str) -> Self {
        ServerRef::Name(name.to_string())
    }
}

impl From<String> for ServerRef {
    fn from(name: String) -> Self {
        ServerRef::Name(name)
    }
}

impl From<Arc<dyn ViaRegistry>> for ServerRef {
    fn from(via: Arc<dyn ViaRegistry>) -> Self {
        ServerRef::Via(via)
    }
}

impl std::fmt::Display for ServerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerRef::Pid(pid) => write!(f, "{:?}", pid),
            ServerRef::Name(name) => write!(f, "{}", name),
            ServerRef::Via(_) => write!(f, "<via>"),
        }
    }
}
