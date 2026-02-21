//! Stats collection GenServer.
//!
//! Receives events from other components and aggregates metrics.
//! Uses Timer::send_interval for periodic stats snapshots.

use crate::job::JobId;
use ambitious::Message;
use ambitious::gen_server::async_trait;
use ambitious::gen_server::{From, GenServer, Init, Reply, Status};
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
    Completed { job_id: JobId, duration_ms: u64 },
    /// A job failed (will be retried).
    Failed { job_id: JobId },
    /// A job was sent to retry.
    Retried { job_id: JobId, attempt: u32 },
    /// A job was dead-lettered.
    DeadLettered { job_id: JobId },
    /// A job was enqueued.
    Enqueued,
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
#[allow(dead_code)] // Fields used by TUI in later task
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
            StatsCast::Completed {
                job_id,
                duration_ms,
            } => {
                self.total_completed += 1;
                self.recent_completions.push_back((now, duration_ms));
                self.push_activity(format!("job {} completed ({duration_ms}ms)", job_id));
            }
            StatsCast::Failed { job_id } => {
                self.total_failed += 1;
                self.push_activity(format!("job {} failed", job_id));
            }
            StatsCast::Retried { job_id, attempt } => {
                self.total_retried += 1;
                self.push_activity(format!(
                    "job {} retry {}/{}",
                    job_id,
                    attempt,
                    crate::job::MAX_RETRIES
                ));
            }
            StatsCast::DeadLettered { job_id } => {
                self.total_dead_lettered += 1;
                self.push_activity(format!("job {} -> dead letter", job_id));
            }
            StatsCast::Enqueued => {
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
            self.recent_completions
                .iter()
                .map(|(_, d)| *d as f64)
                .sum::<f64>()
                / count
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
