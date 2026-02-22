//! Dead letter store for jobs that exhausted retries.
//!
//! Wraps ambitious::store::Store in a GenServer for thread-safe access
//! and process-ownership semantics.

use crate::job::{Job, JobId};
use ambitious::Message;
use ambitious::gen_server::async_trait;
use ambitious::gen_server::{From, GenServer, Init, Reply, Status};
use ambitious::store::Store;
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
