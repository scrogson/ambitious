//! ServerRef - unified way to address a GenServer.

use crate::core::Pid;

/// A reference to a GenServer, either by PID or registered name.
///
/// This allows GenServer client functions (`call`, `cast`, `stop`) to
/// accept either a `Pid` or a name string.
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
#[derive(Debug, Clone)]
pub enum ServerRef {
    /// Reference by process ID.
    Pid(Pid),
    /// Reference by registered name.
    Name(String),
}

impl ServerRef {
    /// Resolves the ServerRef to a Pid.
    ///
    /// Returns `None` if the name is not registered.
    pub fn resolve(&self) -> Option<Pid> {
        match self {
            ServerRef::Pid(pid) => Some(*pid),
            ServerRef::Name(name) => crate::whereis(name),
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

impl std::fmt::Display for ServerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerRef::Pid(pid) => write!(f, "{:?}", pid),
            ServerRef::Name(name) => write!(f, "{}", name),
        }
    }
}
