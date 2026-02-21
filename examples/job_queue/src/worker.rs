//! Worker GenServer that processes individual jobs.
//!
//! Workers are spawned by the DynamicSupervisor and receive job assignments
//! from the JobQueue. They simulate image processing with random latency
//! and failure rates.

use crate::job::{Job, JobResult};
use ambitious::Message;
use ambitious::gen_server::async_trait;
use ambitious::gen_server::{From, GenServer, Init, Reply, Status, cast, start_link};
use ambitious::supervisor::dynamic_supervisor;
use ambitious::supervisor::{ChildSpec, RestartType, StartChildError};
use ambitious::{ExitReason, Pid};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum WorkerCall {}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum WorkerCast {
    /// Assign a job to this worker.
    Process(Job),
}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum WorkerInfo {}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum WorkerReply {}

pub struct Worker {
    worker_id: u32,
    queue_pid: Pid,
}

/// Args to start a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerArgs {
    pub worker_id: u32,
    pub queue_pid: Pid,
}

#[async_trait]
impl GenServer for Worker {
    type Args = WorkerArgs;
    type Call = WorkerCall;
    type Cast = WorkerCast;
    type Info = WorkerInfo;
    type Reply = WorkerReply;

    async fn init(args: WorkerArgs) -> Init<Self> {
        tracing::debug!(worker_id = args.worker_id, "Worker started");
        Init::Ok(Worker {
            worker_id: args.worker_id,
            queue_pid: args.queue_pid,
        })
    }

    async fn handle_call(&mut self, _msg: WorkerCall, _from: From) -> Reply<WorkerReply> {
        Reply::NoReply
    }

    async fn handle_cast(&mut self, msg: WorkerCast) -> Status {
        match msg {
            WorkerCast::Process(job) => {
                let result = self.process_job(&job).await;
                // Report result back to queue
                cast::<crate::queue::JobQueue>(
                    self.queue_pid,
                    crate::queue::QueueCast::JobFinished {
                        worker_pid: ambitious::current_pid(),
                        result,
                    },
                );
                Status::Ok
            }
        }
    }

    async fn handle_info(&mut self, _msg: WorkerInfo) -> Status {
        Status::Ok
    }

    async fn terminate(&mut self, _reason: ExitReason) {
        tracing::debug!(worker_id = self.worker_id, "Worker terminated");
    }
}

impl Worker {
    async fn process_job(&self, job: &Job) -> JobResult {
        // Compute random values before any await points since ThreadRng is !Send.
        let (duration_ms, should_crash, should_fail) = {
            let mut rng = rand::rng();

            // Simulate variable processing time: base +/- 50%
            let base = job.job_type.base_duration().as_millis() as f64;
            let jitter = base * rng.random_range(-0.5..0.5f64);
            let duration_ms = (base + jitter).max(100.0) as u64;

            // ~5% chance of crash
            let should_crash = rng.random_ratio(1, 20);
            // ~20% chance of failure
            let should_fail = rng.random_ratio(1, 5);

            (duration_ms, should_crash, should_fail)
        };

        // ~5% chance of crash (panic that kills the worker process)
        if should_crash {
            tracing::warn!(
                worker_id = self.worker_id,
                job_id = job.id,
                "Worker crashing!"
            );
            panic!("simulated worker crash");
        }

        // Simulate work
        tokio::time::sleep(Duration::from_millis(duration_ms)).await;

        // ~20% chance of failure
        if should_fail {
            JobResult::Failed {
                job_id: job.id,
                error: "simulated processing error".to_string(),
            }
        } else {
            JobResult::Success {
                job_id: job.id,
                duration_ms,
            }
        }
    }
}

/// Start a worker under the dynamic supervisor.
pub async fn start_worker(sup_pid: Pid, args: WorkerArgs) -> Result<Pid, String> {
    let id = format!("worker_{}", args.worker_id);
    let spec = ChildSpec::new(id, move || {
        let args = args.clone();
        async move {
            start_link::<Worker>(args)
                .await
                .map_err(|e| StartChildError::Failed(format!("{:?}", e)))
        }
    })
    .restart(RestartType::Temporary);

    dynamic_supervisor::start_child(sup_pid, spec)
        .await
        .map_err(|e| format!("{:?}", e))
}
