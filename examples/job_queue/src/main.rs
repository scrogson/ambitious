//! Job Queue example demonstrating production patterns with Ambitious.
//!
//! Patterns demonstrated:
//! - DynamicSupervisor for worker pool management
//! - Backpressure (queue depth limits)
//! - Retry with exponential backoff
//! - Dead letter queue (Store-backed)
//! - Process monitors for crash recovery
//! - Graceful shutdown
//! - Timer-based stats collection

#![deny(warnings)]

#[allow(dead_code)]
mod dead_letter;
#[allow(dead_code)]
mod job;
#[allow(dead_code)]
mod stats;

#[ambitious::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!("Job Queue starting");

    // Placeholder - will be replaced with full app in later tasks
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down");
    Ok(())
}
