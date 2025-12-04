//! Application types and configuration.

use crate::core::Pid;
use std::collections::HashMap;

/// Application configuration as key-value pairs.
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    /// Configuration values.
    values: HashMap<String, ConfigValue>,
}

/// A configuration value.
#[derive(Debug, Clone)]
pub enum ConfigValue {
    /// A string value.
    String(String),
    /// An integer value.
    Integer(i64),
    /// A floating point value.
    Float(f64),
    /// A boolean value.
    Bool(bool),
    /// A list of values.
    List(Vec<ConfigValue>),
    /// A nested configuration.
    Map(HashMap<String, ConfigValue>),
}

impl AppConfig {
    /// Creates a new empty configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a string value.
    pub fn set_string(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values
            .insert(key.into(), ConfigValue::String(value.into()));
    }

    /// Sets an integer value.
    pub fn set_int(&mut self, key: impl Into<String>, value: i64) {
        self.values.insert(key.into(), ConfigValue::Integer(value));
    }

    /// Sets a float value.
    pub fn set_float(&mut self, key: impl Into<String>, value: f64) {
        self.values.insert(key.into(), ConfigValue::Float(value));
    }

    /// Sets a boolean value.
    pub fn set_bool(&mut self, key: impl Into<String>, value: bool) {
        self.values.insert(key.into(), ConfigValue::Bool(value));
    }

    /// Gets a string value.
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(ConfigValue::String(s)) => Some(s),
            _ => None,
        }
    }

    /// Gets an integer value.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.values.get(key) {
            Some(ConfigValue::Integer(i)) => Some(*i),
            _ => None,
        }
    }

    /// Gets a float value.
    pub fn get_float(&self, key: &str) -> Option<f64> {
        match self.values.get(key) {
            Some(ConfigValue::Float(f)) => Some(*f),
            _ => None,
        }
    }

    /// Gets a boolean value.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.values.get(key) {
            Some(ConfigValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// Gets a raw config value.
    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        self.values.get(key)
    }

    /// Returns an iterator over all keys.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.values.keys()
    }
}

/// The result of starting an application.
#[derive(Debug)]
pub enum StartResult {
    /// Application started a supervisor tree.
    Supervisor(Pid),
    /// Application started a single process.
    Process(Pid),
    /// Application has no process (library application).
    None,
}

impl StartResult {
    /// Returns the PID if the application started a process.
    pub fn pid(&self) -> Option<Pid> {
        match self {
            StartResult::Supervisor(pid) | StartResult::Process(pid) => Some(*pid),
            StartResult::None => None,
        }
    }
}

/// Information about a running application.
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// The application name.
    pub name: String,
    /// The application's root PID, if any.
    pub pid: Option<Pid>,
    /// Applications this one depends on.
    pub dependencies: Vec<String>,
    /// Whether the application is currently running.
    pub running: bool,
}

/// Application specification for registration.
#[derive(Debug, Clone)]
pub struct AppSpec {
    /// The application name.
    pub name: String,
    /// Applications this one depends on (must be started first).
    pub dependencies: Vec<String>,
    /// Description of the application.
    pub description: String,
}

impl AppSpec {
    /// Creates a new application specification.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            dependencies: Vec::new(),
            description: String::new(),
        }
    }

    /// Adds a dependency.
    pub fn depends_on(mut self, dep: impl Into<String>) -> Self {
        self.dependencies.push(dep.into());
        self
    }

    /// Sets the description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }
}
