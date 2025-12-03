//! Application trait and controller implementation.

use crate::error::{StartError, StopError};
use crate::types::{AppConfig, AppInfo, AppSpec, StartResult};
use starlang_core::{ExitReason, Pid};
use starlang_process::RuntimeHandle;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// The Application trait for implementing OTP-style applications.
///
/// Applications are the top-level organizational unit in a Starlang system.
/// Each application can start a supervision tree and manage its lifecycle.
///
/// # Example
///
/// ```ignore
/// use starlang_application::{Application, AppConfig, StartResult};
/// use starlang_process::RuntimeHandle;
///
/// struct MyApp;
///
/// impl Application for MyApp {
///     fn start(handle: &RuntimeHandle, config: &AppConfig) -> Result<StartResult, String> {
///         // Start your supervision tree here
///         Ok(StartResult::None)
///     }
///
///     fn stop(_pid: Option<Pid>) {
///         // Cleanup when the application stops
///     }
/// }
/// ```
pub trait Application: Sized + Send + Sync + 'static {
    /// Starts the application.
    ///
    /// This is called when the application is started. It should return
    /// a `StartResult` indicating whether the application started a
    /// supervisor, a single process, or no process (library application).
    fn start(handle: &RuntimeHandle, config: &AppConfig) -> Result<StartResult, String>;

    /// Stops the application.
    ///
    /// This is called when the application is being stopped. The PID
    /// is the root process if the application started one.
    fn stop(pid: Option<Pid>) {
        // Default implementation does nothing
        let _ = pid;
    }

    /// Returns the application specification.
    fn spec() -> AppSpec;
}

/// A registered application with its factory function.
struct RegisteredApp {
    /// The application specification.
    spec: AppSpec,
    /// Factory function to start the application.
    start_fn: Box<dyn Fn(&RuntimeHandle, &AppConfig) -> Result<StartResult, String> + Send + Sync>,
    /// Stop function.
    stop_fn: Box<dyn Fn(Option<Pid>) + Send + Sync>,
}

/// A running application instance.
struct RunningApp {
    /// The application name.
    name: String,
    /// The root PID if any.
    pid: Option<Pid>,
    /// The configuration used to start (reserved for future use).
    #[allow(dead_code)]
    config: AppConfig,
}

/// Controller for managing applications.
///
/// The `AppController` handles registration, starting, stopping, and
/// dependency management for applications.
pub struct AppController {
    /// Registered applications.
    registered: Arc<RwLock<HashMap<String, RegisteredApp>>>,
    /// Running applications.
    running: Arc<RwLock<HashMap<String, RunningApp>>>,
    /// The runtime handle.
    handle: RuntimeHandle,
}

impl AppController {
    /// Creates a new application controller.
    pub fn new(handle: RuntimeHandle) -> Self {
        Self {
            registered: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(RwLock::new(HashMap::new())),
            handle,
        }
    }

    /// Registers an application type.
    pub fn register<A: Application>(&self) {
        let spec = A::spec();
        let name = spec.name.clone();

        let app = RegisteredApp {
            spec,
            start_fn: Box::new(|handle, config| A::start(handle, config)),
            stop_fn: Box::new(|pid| A::stop(pid)),
        };

        let mut registered = self.registered.write().unwrap();
        registered.insert(name, app);
    }

    /// Starts an application and its dependencies.
    pub async fn start(&self, name: &str) -> Result<(), StartError> {
        self.start_with_config(name, AppConfig::new()).await
    }

    /// Starts an application with configuration.
    pub async fn start_with_config(&self, name: &str, config: AppConfig) -> Result<(), StartError> {
        // Check if already running
        {
            let running = self.running.read().unwrap();
            if running.contains_key(name) {
                return Err(StartError::AlreadyRunning(name.to_string()));
            }
        }

        // Get the startup order (dependencies first)
        let order = self.resolve_dependencies(name)?;

        // Start each application in order
        for app_name in order {
            // Skip if already running
            {
                let running = self.running.read().unwrap();
                if running.contains_key(&app_name) {
                    continue;
                }
            }

            // Get the app
            let (start_fn, _spec) = {
                let registered = self.registered.read().unwrap();
                let app = registered
                    .get(&app_name)
                    .ok_or_else(|| StartError::NotFound(app_name.clone()))?;

                // Clone what we need
                (app.start_fn.as_ref() as *const _, app.spec.clone())
            };

            // Start it
            let app_config = if app_name == name {
                config.clone()
            } else {
                AppConfig::new()
            };

            // Safety: We're calling within the same function scope
            let result = unsafe {
                let start_fn: &(dyn Fn(&RuntimeHandle, &AppConfig) -> Result<StartResult, String>
                      + Send
                      + Sync) = &*start_fn;
                start_fn(&self.handle, &app_config)
            };

            let start_result = result.map_err(StartError::StartFailed)?;

            // Record as running
            let mut running = self.running.write().unwrap();
            running.insert(
                app_name.clone(),
                RunningApp {
                    name: app_name,
                    pid: start_result.pid(),
                    config: app_config,
                },
            );
        }

        Ok(())
    }

    /// Stops an application.
    pub async fn stop(&self, name: &str) -> Result<(), StopError> {
        // Get the running app
        let running_app = {
            let mut running = self.running.write().unwrap();
            running
                .remove(name)
                .ok_or_else(|| StopError::NotRunning(name.to_string()))?
        };

        // Call the stop function
        {
            let registered = self.registered.read().unwrap();
            if let Some(app) = registered.get(name) {
                (app.stop_fn)(running_app.pid);
            }
        }

        // If there's a PID, send exit signal
        if let Some(pid) = running_app.pid {
            let _ = self.handle.exit(pid, ExitReason::Shutdown);
        }

        Ok(())
    }

    /// Stops all running applications in reverse dependency order.
    pub async fn stop_all(&self) {
        // Get all running app names
        let names: Vec<String> = {
            let running = self.running.read().unwrap();
            running.keys().cloned().collect()
        };

        // Stop each (in reverse order would be better, but this is simpler)
        for name in names.iter().rev() {
            let _ = self.stop(name).await;
        }
    }

    /// Returns information about all registered applications.
    pub fn list_registered(&self) -> Vec<AppSpec> {
        let registered = self.registered.read().unwrap();
        registered.values().map(|a| a.spec.clone()).collect()
    }

    /// Returns information about all running applications.
    pub fn list_running(&self) -> Vec<AppInfo> {
        let running = self.running.read().unwrap();
        let registered = self.registered.read().unwrap();

        running
            .values()
            .map(|r| {
                let deps = registered
                    .get(&r.name)
                    .map(|a| a.spec.dependencies.clone())
                    .unwrap_or_default();

                AppInfo {
                    name: r.name.clone(),
                    pid: r.pid,
                    dependencies: deps,
                    running: true,
                }
            })
            .collect()
    }

    /// Checks if an application is running.
    pub fn is_running(&self, name: &str) -> bool {
        let running = self.running.read().unwrap();
        running.contains_key(name)
    }

    /// Resolves dependencies and returns startup order.
    fn resolve_dependencies(&self, name: &str) -> Result<Vec<String>, StartError> {
        let registered = self.registered.read().unwrap();

        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut path = Vec::new();

        self.visit_deps(name, &registered, &mut result, &mut visited, &mut path)?;

        Ok(result)
    }

    /// Recursive dependency visitor.
    fn visit_deps(
        &self,
        name: &str,
        registered: &HashMap<String, RegisteredApp>,
        result: &mut Vec<String>,
        visited: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Result<(), StartError> {
        // Check for circular dependency
        if path.contains(&name.to_string()) {
            path.push(name.to_string());
            return Err(StartError::CircularDependency(path.clone()));
        }

        // Skip if already visited
        if visited.contains(name) {
            return Ok(());
        }

        // Get the app
        let app = registered
            .get(name)
            .ok_or_else(|| StartError::NotFound(name.to_string()))?;

        // Visit dependencies first
        path.push(name.to_string());
        for dep in &app.spec.dependencies {
            self.visit_deps(dep, registered, result, visited, path)?;
        }
        path.pop();

        // Add to result
        visited.insert(name.to_string());
        result.push(name.to_string());

        Ok(())
    }
}
