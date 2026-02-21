//! JobQueue GenServer — central job coordinator.
//!
//! Accepts jobs, dispatches to idle workers from the DynamicSupervisor pool,
//! handles completions/failures, and enforces backpressure.

use crate::dead_letter::DeadLetterCast;
use crate::job::{Job, JobResult, MAX_QUEUE_DEPTH, MAX_RETRIES};
use crate::stats::StatsCast;
use crate::worker::{self, WorkerArgs};
use ambitious::Message;
use ambitious::gen_server::async_trait;
use ambitious::gen_server::{From, GenServer, Init, Reply, Status, cast};
use ambitious::{ExitReason, Pid};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

static WORKER_ID_COUNTER: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum QueueCall {
    /// Get current queue status.
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum QueueCast {
    /// Enqueue a new job.
    Enqueue(Job),
    /// Enqueue a batch of jobs.
    EnqueueBatch(Vec<Job>),
    /// A worker finished processing a job.
    JobFinished { worker_pid: Pid, result: JobResult },
    /// Add a new worker to the pool.
    AddWorker,
    /// Remove an idle worker from the pool.
    RemoveWorker,
}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum QueueInfo {}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum QueueReply {
    Status(QueueStatus),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueStatus {
    pub pending: usize,
    pub in_flight: usize,
    pub total_workers: usize,
    pub idle_workers: usize,
}

pub struct JobQueue {
    /// Pending jobs waiting for a worker.
    pending: VecDeque<Job>,
    /// Workers currently processing a job: worker_pid -> job.
    in_flight: HashMap<Pid, Job>,
    /// Idle workers ready for work.
    idle_workers: Vec<Pid>,
    /// DynamicSupervisor PID for the worker pool.
    worker_sup: Pid,
    /// All known worker PIDs (for count).
    all_workers: Vec<Pid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueArgs {
    pub worker_sup: Pid,
    pub initial_workers: u32,
}

#[async_trait]
impl GenServer for JobQueue {
    type Args = QueueArgs;
    type Call = QueueCall;
    type Cast = QueueCast;
    type Info = QueueInfo;
    type Reply = QueueReply;

    async fn init(args: QueueArgs) -> Init<Self> {
        let me = ambitious::current_pid();
        let _ = ambitious::register("job_queue".to_string(), me);

        let mut queue = JobQueue {
            pending: VecDeque::new(),
            in_flight: HashMap::new(),
            idle_workers: Vec::new(),
            worker_sup: args.worker_sup,
            all_workers: Vec::new(),
        };

        // Spawn initial workers
        for _ in 0..args.initial_workers {
            queue.spawn_worker().await;
        }

        Init::Ok(queue)
    }

    async fn handle_call(&mut self, msg: QueueCall, _from: From) -> Reply<QueueReply> {
        match msg {
            QueueCall::Status => Reply::Ok(QueueReply::Status(QueueStatus {
                pending: self.pending.len(),
                in_flight: self.in_flight.len(),
                total_workers: self.all_workers.len(),
                idle_workers: self.idle_workers.len(),
            })),
        }
    }

    async fn handle_cast(&mut self, msg: QueueCast) -> Status {
        match msg {
            QueueCast::Enqueue(job) => {
                self.enqueue(job);
            }
            QueueCast::EnqueueBatch(jobs) => {
                for job in jobs {
                    self.enqueue(job);
                }
            }
            QueueCast::JobFinished { worker_pid, result } => {
                self.handle_job_result(worker_pid, result);
            }
            QueueCast::AddWorker => {
                self.spawn_worker().await;
            }
            QueueCast::RemoveWorker => {
                self.remove_idle_worker();
            }
        }
        Status::Ok
    }

    async fn handle_info(&mut self, _msg: QueueInfo) -> Status {
        Status::Ok
    }

    async fn handle_raw_info(&mut self, msg: Vec<u8>) -> Status {
        // Handle worker DOWN messages (monitor notifications)
        if let Ok(ambitious::core::SystemMessage::Down { pid, reason, .. }) =
            postcard::from_bytes::<ambitious::core::SystemMessage>(&msg)
        {
            tracing::warn!(worker = ?pid, reason = ?reason, "Worker process died");
            // Recover the in-flight job
            if let Some(job) = self.in_flight.remove(&pid) {
                self.all_workers.retain(|w| *w != pid);
                // Treat crash as a failure - send to retry
                self.send_to_retry(job);
            }
            // Spawn a replacement worker
            self.spawn_worker().await;
        }
        Status::Ok
    }

    async fn terminate(&mut self, _reason: ExitReason) {
        tracing::info!(
            pending = self.pending.len(),
            in_flight = self.in_flight.len(),
            "JobQueue shutting down"
        );
    }
}

impl JobQueue {
    fn enqueue(&mut self, job: Job) {
        // Backpressure: reject if queue is full
        if self.pending.len() >= MAX_QUEUE_DEPTH {
            tracing::warn!(job_id = job.id, "Job rejected: queue full (backpressure)");
            return;
        }

        self.report_stat(StatsCast::Enqueued);
        self.pending.push_back(job);
        self.dispatch_next();
    }

    fn dispatch_next(&mut self) {
        while let Some(worker_pid) = self.idle_workers.pop() {
            // Verify worker is still alive
            if !ambitious::alive(worker_pid) {
                self.all_workers.retain(|w| *w != worker_pid);
                continue;
            }
            if let Some(job) = self.pending.pop_front() {
                // Assign job to worker
                cast::<crate::worker::Worker>(
                    worker_pid,
                    crate::worker::WorkerCast::Process(job.clone()),
                );
                self.in_flight.insert(worker_pid, job);
            } else {
                // No more pending jobs, put worker back
                self.idle_workers.push(worker_pid);
                break;
            }
        }
    }

    fn handle_job_result(&mut self, worker_pid: Pid, result: JobResult) {
        let job = self.in_flight.remove(&worker_pid);
        self.idle_workers.push(worker_pid);

        match result {
            JobResult::Success {
                job_id,
                duration_ms,
            } => {
                self.report_stat(StatsCast::Completed {
                    job_id,
                    duration_ms,
                });
            }
            JobResult::Failed { job_id, error } => {
                tracing::debug!(job_id, error = %error, "Job failed");
                self.report_stat(StatsCast::Failed { job_id });
                if let Some(job) = job {
                    self.send_to_retry(job);
                }
            }
            JobResult::Crashed { job_id } => {
                self.report_stat(StatsCast::Failed { job_id });
                if let Some(job) = job {
                    self.send_to_retry(job);
                }
            }
        }

        // Dispatch next pending job
        self.dispatch_next();
    }

    fn send_to_retry(&self, mut job: Job) {
        job.retry_count += 1;
        if job.retry_count > MAX_RETRIES {
            // Dead letter
            self.report_stat(StatsCast::DeadLettered { job_id: job.id });
            if let Some(dl_pid) = ambitious::whereis("dead_letter") {
                cast::<crate::dead_letter::DeadLetterStore>(dl_pid, DeadLetterCast::Add(job));
            }
        } else {
            // Schedule retry with exponential backoff: 1s, 2s, 4s
            self.report_stat(StatsCast::Retried {
                job_id: job.id,
                attempt: job.retry_count,
            });
            let delay = Duration::from_secs(1 << (job.retry_count - 1));
            if let Some(queue_pid) = ambitious::whereis("job_queue") {
                let _ = ambitious::timer::send_after(delay, queue_pid, &QueueCast::Enqueue(job));
            }
        }
    }

    fn report_stat(&self, msg: StatsCast) {
        if let Some(stats_pid) = ambitious::whereis("stats") {
            cast::<crate::stats::StatsCollector>(stats_pid, msg);
        }
    }

    async fn spawn_worker(&mut self) {
        let worker_id = WORKER_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let queue_pid = ambitious::current_pid();
        let args = WorkerArgs {
            worker_id,
            queue_pid,
        };
        match worker::start_worker(self.worker_sup, args).await {
            Ok(pid) => {
                // Monitor the worker so we get notified if it crashes
                let _ = ambitious::monitor(pid);
                self.all_workers.push(pid);
                self.idle_workers.push(pid);
                tracing::debug!(worker_id, pid = ?pid, "Spawned worker");
                // Try to dispatch pending work to new worker
                self.dispatch_next();
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to spawn worker");
            }
        }
    }

    fn remove_idle_worker(&mut self) {
        if let Some(pid) = self.idle_workers.pop() {
            self.all_workers.retain(|w| *w != pid);
            let _ =
                ambitious::supervisor::dynamic_supervisor::terminate_child(self.worker_sup, pid);
        }
    }
}
