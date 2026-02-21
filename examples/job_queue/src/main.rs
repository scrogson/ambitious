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
//! - Live TUI dashboard

#![deny(warnings)]

mod dead_letter;
mod job;
mod queue;
mod stats;
mod ui;
mod worker;

use clap::Parser;
use job::{Job, random_filename, random_job_type};
use queue::{QueueArgs, QueueCast};
use std::sync::atomic::{AtomicU64, Ordering};

pub static JOB_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Parser)]
#[command(name = "job-queue", about = "Ambitious Job Queue Example")]
struct Cli {
    /// Number of initial workers
    #[arg(long, default_value = "4")]
    workers: u32,
}

#[ambitious::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!(workers = cli.workers, "Starting Job Queue");

    // Start StatsCollector
    let _stats_pid = ambitious::gen_server::start::<stats::StatsCollector>(())
        .await
        .expect("failed to start StatsCollector");

    // Start DeadLetterStore
    let _dl_pid = ambitious::gen_server::start::<dead_letter::DeadLetterStore>(())
        .await
        .expect("failed to start DeadLetterStore");

    // Start worker DynamicSupervisor
    let worker_sup = ambitious::supervisor::dynamic_supervisor::start_link(
        ambitious::supervisor::dynamic_supervisor::DynamicSupervisorOpts::new()
            .max_restarts(10)
            .max_seconds(5),
    )
    .await
    .expect("failed to start worker supervisor");

    // Start JobQueue
    let queue_pid = ambitious::gen_server::start::<queue::JobQueue>(QueueArgs {
        worker_sup,
        initial_workers: cli.workers,
    })
    .await
    .expect("failed to start JobQueue");

    tracing::info!("Job Queue ready. Enqueuing sample jobs...");

    // Auto-enqueue jobs periodically for demo
    let _producer = ambitious::spawn(move || async move {
        loop {
            let batch: Vec<Job> = (0..5)
                .map(|_| {
                    let id = JOB_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
                    Job::new(id, random_job_type(), random_filename())
                })
                .collect();
            ambitious::gen_server::cast::<queue::JobQueue>(
                queue_pid,
                QueueCast::EnqueueBatch(batch),
            );
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });

    // Run the TUI dashboard in a process context (gen_server::call needs current_pid)
    let (tui_done_tx, tui_done_rx) = tokio::sync::oneshot::channel::<()>();
    ambitious::spawn(move || async move {
        if let Err(e) = ui::run_tui(&JOB_ID_COUNTER).await {
            tracing::error!(error = %e, "TUI error");
        }
        let _ = tui_done_tx.send(());
    });

    // Wait for the TUI process to exit
    let _ = tui_done_rx.await;
    tracing::info!("Shutting down");
    Ok(())
}
