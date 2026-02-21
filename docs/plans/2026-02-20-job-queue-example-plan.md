# Job Queue Example Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build an image processing job queue example with TUI dashboard demonstrating DynamicSupervisor, Application lifecycle, Store, Timer, backpressure, retry with backoff, and graceful shutdown.

**Architecture:** A supervision tree rooted in an Application, with a JobQueue GenServer dispatching work to a DynamicSupervisor-managed worker pool. Failed jobs route through a RetryManager with exponential backoff, and exhausted jobs land in a Store-backed dead letter queue. A StatsCollector aggregates metrics on a timer interval for the TUI.

**Tech Stack:** ambitious (with no extra features needed), ratatui + crossterm for TUI, clap for CLI args, tracing for logging, rand for job simulation.

---

### Task 1: Project scaffold

**Files:**
- Create: `examples/job_queue/Cargo.toml`
- Create: `examples/job_queue/src/main.rs`
- Create: `examples/job_queue/src/job.rs`

**Step 1: Create Cargo.toml**

```toml
[package]
name = "ambitious-job-queue"
version.workspace = true
edition = "2024"
description = "Job queue / worker pool example using Ambitious"
license.workspace = true
publish = false

[[bin]]
name = "job-queue"
path = "src/main.rs"

[dependencies]
ambitious = { version = "0.1.0", path = "../../crates/ambitious" }

tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
postcard = { version = "1", features = ["alloc"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
rand = "0.9"

# TUI
ratatui = "0.29"
crossterm = { version = "0.29", features = ["event-stream"] }
futures = "0.3"
```

**Step 2: Create job.rs with types**

```rust
//! Job types and simulation logic.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Unique job identifier.
pub type JobId = u64;

/// Types of image processing operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobType {
    Resize,
    Thumbnail,
    Watermark,
    Compress,
}

impl JobType {
    /// Simulated base processing time for this job type.
    pub fn base_duration(&self) -> Duration {
        match self {
            JobType::Resize => Duration::from_millis(800),
            JobType::Thumbnail => Duration::from_millis(400),
            JobType::Watermark => Duration::from_millis(1200),
            JobType::Compress => Duration::from_millis(600),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            JobType::Resize => "resize",
            JobType::Thumbnail => "thumbnail",
            JobType::Watermark => "watermark",
            JobType::Compress => "compress",
        }
    }
}

/// A job to be processed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub job_type: JobType,
    pub filename: String,
    pub retry_count: u32,
}

impl Job {
    pub fn new(id: JobId, job_type: JobType, filename: String) -> Self {
        Self {
            id,
            job_type,
            filename,
            retry_count: 0,
        }
    }
}

/// Maximum number of retries before dead-lettering.
pub const MAX_RETRIES: u32 = 3;

/// Maximum queue depth before backpressure kicks in.
pub const MAX_QUEUE_DEPTH: usize = 100;

/// Result of processing a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobResult {
    Success { job_id: JobId, duration_ms: u64 },
    Failed { job_id: JobId, error: String },
    Crashed { job_id: JobId },
}

/// Generate a random filename like "photo_042.jpg".
pub fn random_filename() -> String {
    use rand::Rng;
    let n: u32 = rand::rng().random_range(1..=999);
    format!("photo_{:03}.jpg", n)
}

/// Pick a random job type.
pub fn random_job_type() -> JobType {
    use rand::Rng;
    match rand::rng().random_range(0..4u8) {
        0 => JobType::Resize,
        1 => JobType::Thumbnail,
        2 => JobType::Watermark,
        _ => JobType::Compress,
    }
}
```

**Step 3: Create minimal main.rs**

```rust
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

mod job;

#[ambitious::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!("Job Queue starting");

    // Placeholder - will be replaced with full app in later tasks
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down");
    Ok(())
}
```

**Step 4: Verify it builds**

Run: `cargo check -p ambitious-job-queue`
Expected: compiles with no errors.

**Step 5: Commit**

```
git add examples/job_queue/
git commit -m "Add job queue example scaffold with job types"
```

---

### Task 2: StatsCollector GenServer

The simplest GenServer — no dependencies on other components. Good foundation to build on.

**Files:**
- Create: `examples/job_queue/src/stats.rs`
- Modify: `examples/job_queue/src/main.rs` (add `mod stats;`)

**Step 1: Create stats.rs**

```rust
//! Stats collection GenServer.
//!
//! Receives events from other components and aggregates metrics.
//! Uses Timer::send_interval for periodic stats snapshots.

use crate::job::JobId;
use ambitious::gen_server::{self, From, GenServer, Init, Reply, Status};
use ambitious::Message;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Call messages for StatsCollector.
#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum StatsCall {
    /// Get current stats snapshot.
    GetSnapshot,
}

/// Cast messages for StatsCollector.
#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum StatsCast {
    /// A job was completed successfully.
    JobCompleted { job_id: JobId, duration_ms: u64 },
    /// A job failed (will be retried).
    JobFailed { job_id: JobId },
    /// A job was sent to retry.
    JobRetried { job_id: JobId, attempt: u32 },
    /// A job was dead-lettered.
    JobDeadLettered { job_id: JobId },
    /// A job was enqueued.
    JobEnqueued,
}

/// Info messages for StatsCollector.
#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum StatsInfo {
    /// Timer tick — compute rolling stats.
    Tick,
}

/// Reply type.
#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum StatsReply {
    Snapshot(StatsSnapshot),
}

/// A point-in-time stats snapshot for the TUI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsSnapshot {
    pub total_completed: u64,
    pub total_failed: u64,
    pub total_retried: u64,
    pub total_dead_lettered: u64,
    pub total_enqueued: u64,
    /// Jobs completed per second (rolling 10s window).
    pub throughput: f64,
    /// Average job duration in ms (rolling 10s window).
    pub avg_duration_ms: f64,
}

/// Recent activity entry for the TUI log.
#[derive(Debug, Clone)]
pub struct ActivityEntry {
    pub timestamp: Instant,
    pub message: String,
}

pub struct StatsCollector {
    total_completed: u64,
    total_failed: u64,
    total_retried: u64,
    total_dead_lettered: u64,
    total_enqueued: u64,
    /// Rolling window of (timestamp, duration_ms) for throughput/avg calculation.
    recent_completions: VecDeque<(Instant, u64)>,
    /// Recent activity log for TUI display.
    pub activity: VecDeque<ActivityEntry>,
    /// Rolling window size.
    window: Duration,
    /// Computed snapshot (updated on tick).
    snapshot: StatsSnapshot,
}

#[async_trait]
impl GenServer for StatsCollector {
    type Args = ();
    type Call = StatsCall;
    type Cast = StatsCast;
    type Info = StatsInfo;
    type Reply = StatsReply;

    async fn init(_args: ()) -> Init<Self> {
        let me = ambitious::current_pid();
        let _ = ambitious::register("stats".to_string(), me);

        // Tick every 500ms to update rolling stats
        let _ = ambitious::timer::send_interval(Duration::from_millis(500), me, &StatsInfo::Tick);

        Init::Ok(StatsCollector {
            total_completed: 0,
            total_failed: 0,
            total_retried: 0,
            total_dead_lettered: 0,
            total_enqueued: 0,
            recent_completions: VecDeque::new(),
            activity: VecDeque::new(),
            window: Duration::from_secs(10),
            snapshot: StatsSnapshot::default(),
        })
    }

    async fn handle_call(&mut self, msg: StatsCall, _from: From) -> Reply<StatsReply> {
        match msg {
            StatsCall::GetSnapshot => Reply::Ok(StatsReply::Snapshot(self.snapshot.clone())),
        }
    }

    async fn handle_cast(&mut self, msg: StatsCast) -> Status {
        let now = Instant::now();
        match msg {
            StatsCast::JobCompleted { job_id, duration_ms } => {
                self.total_completed += 1;
                self.recent_completions.push_back((now, duration_ms));
                self.push_activity(format!("✓ job {} completed ({duration_ms}ms)", job_id));
            }
            StatsCast::JobFailed { job_id } => {
                self.total_failed += 1;
                self.push_activity(format!("✗ job {} failed", job_id));
            }
            StatsCast::JobRetried { job_id, attempt } => {
                self.total_retried += 1;
                self.push_activity(format!(
                    "↻ job {} retry {}/{}",
                    job_id,
                    attempt,
                    crate::job::MAX_RETRIES
                ));
            }
            StatsCast::JobDeadLettered { job_id } => {
                self.total_dead_lettered += 1;
                self.push_activity(format!("☠ job {} → dead letter", job_id));
            }
            StatsCast::JobEnqueued => {
                self.total_enqueued += 1;
            }
        }
        Status::Ok
    }

    async fn handle_info(&mut self, msg: StatsInfo) -> Status {
        match msg {
            StatsInfo::Tick => {
                self.compute_snapshot();
                Status::Ok
            }
        }
    }
}

impl StatsCollector {
    fn push_activity(&mut self, message: String) {
        self.activity.push_back(ActivityEntry {
            timestamp: Instant::now(),
            message,
        });
        // Keep last 100 entries
        while self.activity.len() > 100 {
            self.activity.pop_front();
        }
    }

    fn compute_snapshot(&mut self) {
        let now = Instant::now();
        let cutoff = now - self.window;

        // Prune old entries
        while self
            .recent_completions
            .front()
            .is_some_and(|(t, _)| *t < cutoff)
        {
            self.recent_completions.pop_front();
        }

        let count = self.recent_completions.len() as f64;
        let throughput = count / self.window.as_secs_f64();
        let avg_duration_ms = if count > 0.0 {
            self.recent_completions.iter().map(|(_, d)| *d as f64).sum::<f64>() / count
        } else {
            0.0
        };

        self.snapshot = StatsSnapshot {
            total_completed: self.total_completed,
            total_failed: self.total_failed,
            total_retried: self.total_retried,
            total_dead_lettered: self.total_dead_lettered,
            total_enqueued: self.total_enqueued,
            throughput,
            avg_duration_ms,
        };
    }
}
```

**Step 2: Add `mod stats;` to main.rs**

**Step 3: Verify it builds**

Run: `cargo check -p ambitious-job-queue`

**Step 4: Commit**

```
git commit -m "Add StatsCollector GenServer with rolling metrics"
```

---

### Task 3: DeadLetterStore GenServer

Simple GenServer wrapping Store for failed jobs.

**Files:**
- Create: `examples/job_queue/src/dead_letter.rs`
- Modify: `examples/job_queue/src/main.rs` (add `mod dead_letter;`)

**Step 1: Create dead_letter.rs**

```rust
//! Dead letter store for jobs that exhausted retries.
//!
//! Wraps ambitious::store::Store in a GenServer for thread-safe access
//! and process-ownership semantics.

use crate::job::{Job, JobId};
use ambitious::gen_server::{self, From, GenServer, Init, Reply, Status};
use ambitious::store::Store;
use ambitious::Message;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum DeadLetterCall {
    /// Get count of dead-lettered jobs.
    Count,
    /// List all dead-lettered jobs.
    List,
}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum DeadLetterCast {
    /// Add a job to the dead letter store.
    Add(Job),
}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum DeadLetterInfo {}

#[derive(Debug, Clone, Serialize, Deserialize, Message)]
pub enum DeadLetterReply {
    Count(usize),
    List(Vec<Job>),
}

pub struct DeadLetterStore {
    store: Store<JobId, Job>,
}

#[async_trait]
impl GenServer for DeadLetterStore {
    type Args = ();
    type Call = DeadLetterCall;
    type Cast = DeadLetterCast;
    type Info = DeadLetterInfo;
    type Reply = DeadLetterReply;

    async fn init(_args: ()) -> Init<Self> {
        let me = ambitious::current_pid();
        let _ = ambitious::register("dead_letter".to_string(), me);

        Init::Ok(DeadLetterStore {
            store: Store::new(),
        })
    }

    async fn handle_call(&mut self, msg: DeadLetterCall, _from: From) -> Reply<DeadLetterReply> {
        match msg {
            DeadLetterCall::Count => {
                let count = self.store.len().unwrap_or(0);
                Reply::Ok(DeadLetterReply::Count(count))
            }
            DeadLetterCall::List => {
                let jobs = self.store.values().unwrap_or_default();
                Reply::Ok(DeadLetterReply::List(jobs))
            }
        }
    }

    async fn handle_cast(&mut self, msg: DeadLetterCast) -> Status {
        match msg {
            DeadLetterCast::Add(job) => {
                tracing::warn!(job_id = job.id, filename = %job.filename, "Job dead-lettered");
                let _ = self.store.insert(job.id, job);
                Status::Ok
            }
        }
    }

    async fn handle_info(&mut self, _msg: DeadLetterInfo) -> Status {
        Status::Ok
    }
}
```

**Step 2: Add `mod dead_letter;` to main.rs, verify build**

**Step 3: Commit**

```
git commit -m "Add DeadLetterStore GenServer wrapping Store"
```

---

### Task 4: Worker GenServer

Workers process individual jobs with simulated latency, failures, and crashes.

**Files:**
- Create: `examples/job_queue/src/worker.rs`
- Modify: `examples/job_queue/src/main.rs` (add `mod worker;`)

**Step 1: Create worker.rs**

```rust
//! Worker GenServer that processes individual jobs.
//!
//! Workers are spawned by the DynamicSupervisor and receive job assignments
//! from the JobQueue. They simulate image processing with random latency
//! and failure rates.

use crate::job::{Job, JobId, JobResult};
use crate::stats::StatsCast;
use ambitious::gen_server::{self, From, GenServer, Init, Reply, Status, cast, start_link};
use ambitious::Message;
use ambitious::{ExitReason, Pid};
use async_trait::async_trait;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

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

/// State visible to the TUI.
#[derive(Debug, Clone)]
pub struct WorkerStatus {
    pub worker_id: u32,
    pub pid: Pid,
    pub current_job: Option<WorkerJobInfo>,
}

#[derive(Debug, Clone)]
pub struct WorkerJobInfo {
    pub job_id: JobId,
    pub filename: String,
    pub job_type_label: String,
    pub started_at: Instant,
    pub estimated_duration: Duration,
}

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
        let mut rng = rand::rng();

        // Simulate variable processing time: base ± 50%
        let base = job.job_type.base_duration().as_millis() as f64;
        let jitter = base * rng.random_range(-0.5..0.5f64);
        let duration_ms = (base + jitter).max(100.0) as u64;

        // ~5% chance of crash (panic that kills the worker process)
        if rng.random_ratio(1, 20) {
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
        if rng.random_ratio(1, 5) {
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
    use ambitious::supervisor::dynamic_supervisor;
    use ambitious::supervisor::ChildSpec;

    let id = format!("worker_{}", args.worker_id);
    let spec = ChildSpec::new(id, move || {
        let args = args.clone();
        async move {
            start_link::<Worker>(args)
                .await
                .map_err(|e| ambitious::supervisor::StartChildError::Failed(format!("{:?}", e)))
        }
    })
    .restart(ambitious::supervisor::RestartType::Temporary);

    dynamic_supervisor::start_child(sup_pid, spec)
        .await
        .map_err(|e| format!("{:?}", e))
}
```

**Step 2: Add `mod worker;` to main.rs, verify build**

Note: This won't fully compile yet because it references `crate::queue::JobQueue` and `crate::queue::QueueCast` which don't exist yet. Add a `#[allow(unused)]` or create the queue module next. Best approach: implement Task 5 immediately after.

**Step 3: Commit (if compiles) or continue to Task 5**

---

### Task 5: JobQueue GenServer

The central coordinator: accepts jobs, dispatches to workers, handles results.

**Files:**
- Create: `examples/job_queue/src/queue.rs`
- Modify: `examples/job_queue/src/main.rs` (add `mod queue;`)

**Step 1: Create queue.rs**

```rust
//! JobQueue GenServer — central job coordinator.
//!
//! Accepts jobs, dispatches to idle workers from the DynamicSupervisor pool,
//! handles completions/failures, and enforces backpressure.

use crate::dead_letter::DeadLetterCast;
use crate::job::{Job, JobId, JobResult, MAX_QUEUE_DEPTH, MAX_RETRIES};
use crate::stats::StatsCast;
use crate::worker::{self, WorkerArgs};
use ambitious::gen_server::{self, From, GenServer, Init, Reply, Status, cast};
use ambitious::Message;
use ambitious::{ExitReason, Pid};
use async_trait::async_trait;
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
    /// Job was rejected due to backpressure.
    Rejected,
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
        if let Ok(sys_msg) = postcard::from_bytes::<ambitious::core::SystemMessage>(&msg) {
            if let ambitious::core::SystemMessage::Down { pid, reason, .. } = sys_msg {
                tracing::warn!(worker = ?pid, reason = ?reason, "Worker process died");
                // Recover the in-flight job
                if let Some(mut job) = self.in_flight.remove(&pid) {
                    self.all_workers.retain(|w| *w != pid);
                    // Treat crash as a failure - send to retry
                    self.send_to_retry(job);
                }
                // Spawn a replacement worker
                self.spawn_worker().await;
            }
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

        self.report_stat(StatsCast::JobEnqueued);
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
                cast::<crate::worker::Worker>(worker_pid, crate::worker::WorkerCast::Process(job.clone()));
                self.in_flight.insert(worker_pid, job);
                // Continue dispatching if more idle workers and pending jobs
            } else {
                // No more pending jobs, put worker back
                self.idle_workers.push(worker_pid);
                break;
            }
        }
    }

    fn handle_job_result(&mut self, worker_pid: Pid, result: JobResult) {
        let _job = self.in_flight.remove(&worker_pid);
        self.idle_workers.push(worker_pid);

        match result {
            JobResult::Success { job_id, duration_ms } => {
                self.report_stat(StatsCast::JobCompleted { job_id, duration_ms });
            }
            JobResult::Failed { job_id, error } => {
                tracing::debug!(job_id, error = %error, "Job failed");
                self.report_stat(StatsCast::JobFailed { job_id });
                if let Some(job) = _job {
                    self.send_to_retry(job);
                }
            }
            JobResult::Crashed { job_id } => {
                self.report_stat(StatsCast::JobFailed { job_id });
                if let Some(job) = _job {
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
            self.report_stat(StatsCast::JobDeadLettered { job_id: job.id });
            if let Some(dl_pid) = ambitious::whereis("dead_letter") {
                cast::<crate::dead_letter::DeadLetterStore>(dl_pid, DeadLetterCast::Add(job));
            }
        } else {
            // Schedule retry with exponential backoff
            self.report_stat(StatsCast::JobRetried {
                job_id: job.id,
                attempt: job.retry_count,
            });
            let delay = Duration::from_secs(1 << (job.retry_count - 1)); // 1s, 2s, 4s
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
            let _ = ambitious::supervisor::dynamic_supervisor::terminate_child(
                self.worker_sup,
                pid,
            );
        }
    }
}
```

**Step 2: Verify both worker.rs and queue.rs compile together**

Run: `cargo check -p ambitious-job-queue`

**Step 3: Commit**

```
git commit -m "Add JobQueue and Worker GenServers with dispatch and retry"
```

---

### Task 6: Wire up the supervision tree in main.rs

Connect all components with DynamicSupervisor and start accepting jobs.

**Files:**
- Modify: `examples/job_queue/src/main.rs`

**Step 1: Update main.rs**

```rust
//! Job Queue example demonstrating production patterns with Ambitious.

#![deny(warnings)]

mod dead_letter;
mod job;
mod queue;
mod stats;
mod worker;

use clap::Parser;
use job::{random_filename, random_job_type, Job};
use queue::{QueueCast, QueueArgs};
use std::sync::atomic::{AtomicU64, Ordering};

static JOB_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
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

    // Wait for Ctrl-C (TUI will replace this in Task 7)
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down");
    Ok(())
}
```

**Step 2: Build and run to verify components work**

Run: `cargo run -p ambitious-job-queue -- --workers 4`
Expected: Logs showing jobs being enqueued, processed, completed/failed/retried.

**Step 3: Commit**

```
git commit -m "Wire up supervision tree and job producer in main"
```

---

### Task 7: TUI dashboard

Replace the Ctrl-C wait loop with a ratatui-based dashboard.

**Files:**
- Create: `examples/job_queue/src/ui.rs`
- Modify: `examples/job_queue/src/main.rs` (replace the Ctrl-C block with TUI)

**Step 1: Create ui.rs**

This is the largest file. It should:
- Poll stats via `gen_server::call::<StatsCollector>` every 200ms
- Poll queue status via `gen_server::call::<JobQueue>` every 200ms
- Handle keyboard input: `q` quit, `+` add 10 jobs, `=` add worker, `-` remove worker
- Render the dashboard layout from the design doc

The TUI structure should follow the chat client's pattern:
- `crossterm` for terminal raw mode
- `ratatui` for rendering
- `futures::StreamExt` for event stream
- `tokio::select!` for multiplexing input events and timer ticks

Key sections to render:
1. **Header** — title and quit hint
2. **Queue bar** — pending / in-flight / completed / DLQ counts
3. **Worker panel** — list of workers with status (idle or job progress)
4. **Throughput bar** — jobs/sec, avg duration, error count
5. **Activity log** — scrolling list of recent events

Use `gen_server::call` with short timeouts to poll state. If the call times out, use the last known state.

**Step 2: Update main.rs to launch TUI instead of Ctrl-C wait**

Replace the `tokio::signal::ctrl_c()` block with the TUI event loop. The auto-producer should still run in the background.

**Step 3: Build and test interactively**

Run: `cargo run -p ambitious-job-queue -- --workers 4`
Expected: TUI dashboard showing live job processing.

**Step 4: Commit**

```
git commit -m "Add TUI dashboard with live job processing display"
```

---

### Task 8: Polish and final commit

**Files:**
- All files in `examples/job_queue/`

**Step 1: Run `just ci`**

Fix any fmt, clippy, or test issues.

**Step 2: Test the full demo flow**

1. Start with `cargo run -p ambitious-job-queue -- --workers 2`
2. Watch jobs process with 2 workers
3. Press `=` to add workers, verify throughput increases
4. Press `-` to remove workers
5. Press `+` to flood the queue, verify backpressure
6. Watch retries and dead letters accumulate
7. Press `q` to quit cleanly

**Step 3: Final commit**

```
git commit -m "Polish job queue example"
```

---

## Task Summary

| Task | Component | Key Patterns |
|------|-----------|-------------|
| 1 | Project scaffold + job types | Workspace setup, data modeling |
| 2 | StatsCollector | GenServer, Timer::send_interval, process registration |
| 3 | DeadLetterStore | GenServer, Store |
| 4 | Worker | GenServer, simulated work, crash simulation |
| 5 | JobQueue | GenServer, process monitors, backpressure, retry/backoff, DynamicSupervisor |
| 6 | Main + supervision tree | DynamicSupervisor, wiring, job producer |
| 7 | TUI dashboard | ratatui, crossterm, polling GenServers |
| 8 | Polish | CI compliance, integration testing |
