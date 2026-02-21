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

    #[allow(dead_code)]
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
