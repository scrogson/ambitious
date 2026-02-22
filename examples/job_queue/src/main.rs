//! Job Queue example demonstrating production patterns with Ambitious.
//!
//! Patterns demonstrated:
//! - Supervision tree with rest_for_one strategy
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

use ambitious::gen_server::StartOpts;
use ambitious::supervisor::dynamic_supervisor::{self, DynamicSupervisorOpts};
use ambitious::supervisor::{
    ChildSpec, RestartType, StartChildError, Strategy, Supervisor, SupervisorFlags, SupervisorInit,
};
use ambitious::{Name, gen_server, spawn};
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

/// Top-level supervisor using rest_for_one strategy.
///
/// Children start sequentially and depend on upstream siblings:
///   stats_collector → dead_letter → worker_sup → job_queue → producer
///
/// If any child crashes, it and all children started after it are restarted,
/// ensuring downstream dependents pick up fresh PIDs via process registration.
struct AppSupervisor;

impl Supervisor for AppSupervisor {
    type InitArg = u32; // initial_workers

    fn init(initial_workers: u32) -> SupervisorInit {
        SupervisorInit::new(
            SupervisorFlags::new(Strategy::RestForOne)
                .max_restarts(10)
                .max_seconds(5),
            vec![
                // 1. StatsCollector
                ChildSpec::new("stats_collector", || async {
                    StartOpts::new(())
                        .name(Name::local("stats"))
                        .link()
                        .start::<stats::StatsCollector>()
                        .await
                        .map_err(|e| StartChildError::Failed(e.to_string()))
                })
                .restart(RestartType::Permanent),
                // 2. DeadLetterStore
                ChildSpec::new("dead_letter", || async {
                    StartOpts::new(())
                        .name(Name::local("dead_letter"))
                        .link()
                        .start::<dead_letter::DeadLetterStore>()
                        .await
                        .map_err(|e| StartChildError::Failed(e.to_string()))
                })
                .restart(RestartType::Permanent),
                // 3. Worker DynamicSupervisor (registered as "worker_sup")
                ChildSpec::new("worker_sup", || async {
                    let pid = dynamic_supervisor::start_link(
                        DynamicSupervisorOpts::new()
                            .max_restarts(10)
                            .max_seconds(5)
                            .name(ambitious::Name::local("worker_sup")),
                    )
                    .await
                    .map_err(|e| StartChildError::Failed(e.to_string()))?;
                    Ok(pid)
                })
                .restart(RestartType::Permanent)
                .supervisor(),
                // 4. JobQueue (looks up "worker_sup" via whereis in its init)
                {
                    let workers = initial_workers;
                    ChildSpec::new("job_queue", move || async move {
                        StartOpts::new(QueueArgs {
                            initial_workers: workers,
                        })
                        .name(Name::local("job_queue"))
                        .link()
                        .start::<queue::JobQueue>()
                        .await
                        .map_err(|e| StartChildError::Failed(e.to_string()))
                    })
                    .restart(RestartType::Permanent)
                },
                // 5. Producer (enqueues jobs periodically, looks up "job_queue" by name)
                ChildSpec::new("producer", || async {
                    let pid = spawn(move || async move {
                        loop {
                            let batch: Vec<Job> = (0..5)
                                .map(|_| {
                                    let id = JOB_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
                                    Job::new(id, random_job_type(), random_filename())
                                })
                                .collect();
                            if let Some(queue_pid) = ambitious::whereis("job_queue") {
                                gen_server::cast::<queue::JobQueue>(
                                    queue_pid,
                                    QueueCast::EnqueueBatch(batch),
                                );
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    });
                    Ok(pid)
                })
                .restart(RestartType::Permanent),
            ],
        )
    }
}

#[ambitious::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Suppress default panic output to prevent stderr from corrupting the TUI.
    // Worker panics (simulated crashes) are already handled by process monitors.
    std::panic::set_hook(Box::new(|_| {}));

    let cli = Cli::parse();

    // Start the supervision tree
    let _sup_pid = ambitious::supervisor::start::<AppSupervisor>(&ambitious::handle(), cli.workers)
        .await
        .expect("failed to start supervisor");

    // Run the TUI dashboard in a process context (gen_server::call needs current_pid).
    // The TUI stays outside the supervisor — it's a UI concern, not a supervised service.
    let (tui_done_tx, tui_done_rx) = tokio::sync::oneshot::channel::<()>();
    spawn(move || async move {
        if let Err(e) = ui::run_tui(&JOB_ID_COUNTER).await {
            tracing::error!(error = %e, "TUI error");
        }
        let _ = tui_done_tx.send(());
    });

    // Wait for the TUI process to exit, then force process termination.
    // The ambitious runtime keeps spawned processes alive, so we exit explicitly.
    let _ = tui_done_rx.await;
    std::process::exit(0);
}
