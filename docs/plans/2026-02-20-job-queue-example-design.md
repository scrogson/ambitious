# Job Queue / Worker Pool Example — Design

## Overview

An image processing job queue with a TUI dashboard. Users submit simulated image resize jobs, a pool of workers processes them with realistic latency/failures, and a live dashboard shows queue depth, worker states, throughput, and retry/dead-letter activity.

## Goal

Demonstrate production patterns that the chat example doesn't cover: DynamicSupervisor, Application lifecycle, Store, advanced Timer usage, process monitors for crash recovery, backpressure, retry with exponential backoff, graceful shutdown, and dead letter queues.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                   Application                           │
│  (JobApp implements Application trait)                  │
└─────────────────────────────────────────────────────────┘
                        │
┌─────────────────────────────────────────────────────────┐
│               Top-Level Supervisor                      │
│  Strategy: OneForOne                                    │
│  Children:                                              │
│    ├── JobQueue (GenServer) — accepts & schedules jobs   │
│    ├── WorkerSupervisor (DynamicSupervisor)             │
│    │     └── Worker 1..N (GenServer, Temporary)         │
│    ├── RetryManager (GenServer) — backoff & reschedule  │
│    ├── DeadLetterStore (GenServer wrapping Store)       │
│    └── StatsCollector (GenServer) — throughput metrics   │
└─────────────────────────────────────────────────────────┘
```

## Key Patterns Demonstrated

1. **DynamicSupervisor** — Workers spawned/removed on demand based on load.
2. **Backpressure** — JobQueue rejects when queue exceeds max depth.
3. **Retry with exponential backoff** — RetryManager uses `Timer::send_after` to reschedule failed jobs (1s, 2s, 4s, max 3 retries).
4. **Dead letter queue** — Jobs that exhaust retries go to Store-backed dead letter storage.
5. **Process monitors** — JobQueue monitors workers to detect crashes mid-job and reschedule.
6. **Graceful shutdown** — Application shutdown drains in-flight jobs before stopping workers.
7. **Store** — Dead letter persistence and job metadata.
8. **Timer** — Retry scheduling, stats collection intervals, worker idle timeouts.

## Job Simulation

- Jobs have random processing time (200ms–2s).
- ~20% failure rate (simulated transient errors).
- ~5% crash rate (worker process dies, supervisor restarts).
- Job types: resize, thumbnail, watermark, compress — different simulated costs.

## TUI Dashboard

```
╔═══════════════════════════════════════════════════════════╗
║  Ambitious Job Queue                           [q] quit  ║
╠═══════════════════════════════════════════════════════════╣
║  Queue: 12 pending │ 4 in-flight │ 47 completed │ 3 DLQ ║
╠═══════════════════════════════════════════════════════════╣
║  Workers (4/8)                                           ║
║    #1 [████████░░] resize photo_001.jpg      1.2s        ║
║    #2 [██████░░░░] thumbnail photo_042.jpg   0.8s        ║
║    #3 [idle]                                             ║
║    #4 [██░░░░░░░░] compress photo_007.jpg    0.3s        ║
╠═══════════════════════════════════════════════════════════╣
║  Throughput: 12.4 jobs/sec │ Avg: 850ms │ Errors: 6     ║
║  Retries: 3 pending │ Dead letters: 3                    ║
╠═══════════════════════════════════════════════════════════╣
║  Recent Activity                                         ║
║    10:03:42 ✓ photo_039.jpg resized (1.1s)              ║
║    10:03:41 ✗ photo_038.jpg failed, retry 2/3           ║
║    10:03:41 ✓ photo_037.jpg compressed (0.4s)           ║
║    10:03:40 ☠ photo_033.jpg → dead letter (3/3 retries) ║
╚═══════════════════════════════════════════════════════════╝
  [+] Add 10 jobs  [-] Remove worker  [=] Add worker
```

## Crate Structure

```
examples/job_queue/
├── Cargo.toml
└── src/
    ├── main.rs              # Application setup, TUI event loop
    ├── app.rs               # JobApp (Application trait)
    ├── queue.rs             # JobQueue GenServer
    ├── worker.rs            # Worker GenServer
    ├── worker_supervisor.rs # DynamicSupervisor for workers
    ├── retry.rs             # RetryManager with exponential backoff
    ├── dead_letter.rs       # Dead letter Store wrapper
    ├── stats.rs             # StatsCollector with Timer intervals
    ├── job.rs               # Job types and simulation logic
    └── ui.rs                # TUI rendering with ratatui
```

## Components

### JobQueue (GenServer)

- Maintains a VecDeque of pending jobs.
- On `Enqueue` cast: adds job, dispatches to idle workers if available. Rejects if queue exceeds max depth (backpressure).
- On worker completion notification: dispatches next pending job.
- Monitors each worker it assigns a job to. On `NodeDown`/process exit: reschedules the job via RetryManager.

### Worker (GenServer, Temporary restart)

- Receives a job assignment via cast.
- Simulates processing with `tokio::time::sleep` for random duration.
- Reports completion or failure back to JobQueue.
- ~5% chance of panic (simulates crash) — DynamicSupervisor handles restart.

### WorkerSupervisor (DynamicSupervisor)

- Manages the worker pool.
- Supports adding/removing workers at runtime via TUI commands.
- Workers are `Temporary` restart type (crashed workers are replaced by JobQueue re-dispatching, not auto-restarted with old state).

### RetryManager (GenServer)

- Tracks retry count per job.
- Uses `Timer::send_after` with exponential backoff: 1s, 2s, 4s.
- After max retries (3), sends job to DeadLetterStore.
- On timer fire: re-enqueues job in JobQueue.

### DeadLetterStore (GenServer wrapping Store)

- Persists failed jobs using the `Store` API.
- Queryable for the TUI dashboard display.

### StatsCollector (GenServer)

- Receives events: job_completed, job_failed, job_retried, job_dead_lettered.
- Uses `Timer::send_interval` for periodic stats aggregation.
- Provides current stats snapshot via `call` for TUI polling.

### JobApp (Application trait)

- `start/1` builds the supervision tree and starts all components.
- `stop/1` implements graceful shutdown: stops accepting new jobs, waits for in-flight jobs to drain, then stops workers.
