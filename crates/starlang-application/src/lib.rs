//! # starlang-application
//!
//! Application lifecycle management for Starlang.
//!
//! This crate provides the `Application` trait and `AppController` for
//! managing OTP-style applications with dependency resolution and
//! graceful startup/shutdown.
//!
//! # Overview
//!
//! Applications are the top-level organizational unit. Each application:
//! - Has a specification with name and dependencies
//! - Can start a supervision tree or single process
//! - Is started after its dependencies
//! - Is stopped before its dependents
//!
//! # Example
//!
//! ```ignore
//! use starlang_application::{Application, AppConfig, AppSpec, StartResult, AppController};
//! use starlang_process::RuntimeHandle;
//!
//! struct MyApp;
//!
//! impl Application for MyApp {
//!     fn start(handle: &RuntimeHandle, config: &AppConfig) -> Result<StartResult, String> {
//!         // Start your supervision tree
//!         Ok(StartResult::None)
//!     }
//!
//!     fn spec() -> AppSpec {
//!         AppSpec::new("my_app")
//!             .depends_on("logger")
//!             .description("My application")
//!     }
//! }
//!
//! // Using the controller
//! let controller = AppController::new(handle);
//! controller.register::<MyApp>();
//! controller.start("my_app").await?;
//! ```

#![deny(warnings)]
#![deny(missing_docs)]

mod application;
mod error;
mod types;

pub use application::{AppController, Application};
pub use error::{StartError, StopError};
pub use types::{AppConfig, AppInfo, AppSpec, ConfigValue, StartResult};

// Re-export commonly used types
pub use starlang_core::Pid;
pub use starlang_process::RuntimeHandle;

#[cfg(test)]
mod tests {
    use super::*;
    use starlang_process::Runtime;

    struct TestApp;

    impl Application for TestApp {
        fn start(_handle: &RuntimeHandle, _config: &AppConfig) -> Result<StartResult, String> {
            Ok(StartResult::None)
        }

        fn spec() -> AppSpec {
            AppSpec::new("test_app").description("A test application")
        }
    }

    #[tokio::test]
    async fn test_register_and_start() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        controller.register::<TestApp>();
        assert!(!controller.is_running("test_app"));

        controller.start("test_app").await.unwrap();
        assert!(controller.is_running("test_app"));

        controller.stop("test_app").await.unwrap();
        assert!(!controller.is_running("test_app"));
    }

    #[tokio::test]
    async fn test_start_not_found() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        let result = controller.start("nonexistent").await;
        assert!(matches!(result, Err(StartError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_already_running() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        controller.register::<TestApp>();
        controller.start("test_app").await.unwrap();

        let result = controller.start("test_app").await;
        assert!(matches!(result, Err(StartError::AlreadyRunning(_))));
    }

    #[tokio::test]
    async fn test_dependency_resolution() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        static START_ORDER: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());

        struct BaseApp;
        impl Application for BaseApp {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                START_ORDER.lock().unwrap().push("base");
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("base")
            }
        }

        struct MiddleApp;
        impl Application for MiddleApp {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                START_ORDER.lock().unwrap().push("middle");
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("middle").depends_on("base")
            }
        }

        struct TopApp;
        impl Application for TopApp {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                START_ORDER.lock().unwrap().push("top");
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("top").depends_on("middle")
            }
        }

        controller.register::<BaseApp>();
        controller.register::<MiddleApp>();
        controller.register::<TopApp>();

        // Clear any previous runs
        START_ORDER.lock().unwrap().clear();

        controller.start("top").await.unwrap();

        let order = START_ORDER.lock().unwrap();
        assert_eq!(*order, vec!["base", "middle", "top"]);
    }

    #[tokio::test]
    async fn test_circular_dependency() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        struct AppA;
        impl Application for AppA {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("app_a").depends_on("app_b")
            }
        }

        struct AppB;
        impl Application for AppB {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("app_b").depends_on("app_a")
            }
        }

        controller.register::<AppA>();
        controller.register::<AppB>();

        let result = controller.start("app_a").await;
        assert!(matches!(result, Err(StartError::CircularDependency(_))));
    }

    #[tokio::test]
    async fn test_app_config() {
        let mut config = AppConfig::new();
        config.set_string("name", "test");
        config.set_int("port", 8080);
        config.set_float("ratio", 0.5);
        config.set_bool("enabled", true);

        assert_eq!(config.get_string("name"), Some("test"));
        assert_eq!(config.get_int("port"), Some(8080));
        assert_eq!(config.get_float("ratio"), Some(0.5));
        assert_eq!(config.get_bool("enabled"), Some(true));
        assert_eq!(config.get_string("missing"), None);
    }

    #[tokio::test]
    async fn test_list_running() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        controller.register::<TestApp>();
        controller.start("test_app").await.unwrap();

        let running = controller.list_running();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].name, "test_app");
        assert!(running[0].running);
    }

    #[tokio::test]
    async fn test_stop_all() {
        let runtime = Runtime::new();
        let handle = runtime.handle();
        let controller = AppController::new(handle);

        struct App1;
        impl Application for App1 {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("app1")
            }
        }

        struct App2;
        impl Application for App2 {
            fn start(_: &RuntimeHandle, _: &AppConfig) -> Result<StartResult, String> {
                Ok(StartResult::None)
            }
            fn spec() -> AppSpec {
                AppSpec::new("app2")
            }
        }

        controller.register::<App1>();
        controller.register::<App2>();

        controller.start("app1").await.unwrap();
        controller.start("app2").await.unwrap();

        assert!(controller.is_running("app1"));
        assert!(controller.is_running("app2"));

        controller.stop_all().await;

        assert!(!controller.is_running("app1"));
        assert!(!controller.is_running("app2"));
    }
}
