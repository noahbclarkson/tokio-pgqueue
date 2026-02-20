# tokio-pgqueue

A Postgres-backed job queue for Rust with heartbeat-based job claims, crash recovery, exponential backoff for retries, scheduled jobs, job priority, and dead letter queue support.

## Features

- **Heartbeat-based job claims** - Workers claim jobs and periodically send heartbeats to indicate they're still alive
- **Crash recovery** - Orphaned jobs (where the worker crashed) can be automatically reclaimed
- **Exponential backoff** - Failed jobs are automatically retried with increasing delays
- **Scheduled jobs** - Schedule jobs to run at a specific time or after a delay
- **Job priority** - Assign priorities to jobs (lower value = higher priority)
- **Dead letter queue** - Failed jobs are moved to DLQ for inspection and requeue
- **Tokio-native** - Built on tokio and sqlx for full async support
- **Skip-locked claiming** - Uses `SELECT FOR UPDATE SKIP LOCKED` for efficient, non-blocking job claiming

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
tokio-pgqueue = "0.1"
sqlx = { version = "0.8", features = ["postgres", "runtime-tokio", "chrono", "uuid", "json"] }
tokio = { version = "1", features = ["full"] }
```

## Quick Start

### Basic Usage

```rust
use tokio_pgqueue::{PgQueue, QueueConfig};
use sqlx::PgPool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a database connection pool
    let pool = PgPool::connect("postgres://user:pass@localhost/db").await?;

    // Create the queue with default configuration
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Enqueue a job
    let job_id = queue.enqueue(
        "my_queue",
        "process_image",
        serde_json::json!({"path": "/tmp/img.jpg"})
    ).await?;

    println!("Enqueued job: {}", job_id);

    // Claim a job for processing
    if let Some(job) = queue.claim("my_queue", "worker-1").await? {
        println!("Claimed job: {:?}", job);

        // Send periodic heartbeats while processing
        queue.heartbeat(job.id, "worker-1").await?;

        // Process the job...
        process_job(&job).await?;

        // Mark as completed
        queue.complete(job.id, "worker-1").await?;
    }

    Ok(())
}

async fn process_job(job: &tokio_pgqueue::Job) -> Result<(), Box<dyn std::error::Error>> {
    // Your job processing logic here
    println!("Processing job {} of type {}", job.id, job.job_type);
    Ok(())
}
```

### Using the Worker Abstraction

For long-running workers, use the `QueueWorker` abstraction:

```rust
use tokio_pgqueue::{QueueWorker, WorkerBuilder};
use sqlx::PgPool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect("postgres://user:pass@localhost/db").await?;
    let config = tokio_pgqueue::QueueConfig::default();
    let queue = tokio_pgqueue::PgQueue::new(pool, config).await?;

    let worker = WorkerBuilder::new("my_queue", "worker-1")
        .queue(queue)
        .handler_simple(|job| async move {
            println!("Processing job {}: {:?}", job.id, job.payload);
            // Your processing logic here
            Ok(())
        })
        .build()?;

    // Run the worker indefinitely
    worker.run().await?;

    Ok(())
}
```

## Scheduled Jobs

Schedule jobs to run at a specific time or after a delay:

```rust
use tokio_pgqueue::{PgQueue, EnqueueOptions};
use chrono::{Utc, Duration};

// Schedule for a specific time
let run_at = Utc::now() + Duration::hours(2);
let job_id = queue.enqueue_at(
    "my_queue",
    "scheduled_task",
    serde_json::json!({"task": "cleanup"}),
    run_at
).await?;

// Schedule to run after a delay
let job_id = queue.enqueue_after(
    "my_queue",
    "delayed_task",
    serde_json::json!({"data": "value"}),
    std::time::Duration::from_secs(60)
).await?;

// Or use EnqueueOptions
let options = EnqueueOptions::new()
    .run_after(std::time::Duration::from_secs(300));
let job_id = queue.enqueue_with_options("my_queue", "task", serde_json::json!({}), options).await?;
```

## Job Priority

Jobs can have priorities assigned (0-255, lower value = higher priority):

```rust
use tokio_pgqueue::EnqueueOptions;

// High priority job (priority 0 = highest)
let options = EnqueueOptions::new()
    .priority(10);  // Will be processed before priority 100 jobs
let job_id = queue.enqueue_with_options(
    "my_queue",
    "urgent_task",
    serde_json::json!({"urgent": true}),
    options
).await?;

// Low priority job (default is 100)
let options = EnqueueOptions::new()
    .priority(200);  // Will be processed after lower priority jobs
```

When claiming jobs, workers will get the highest priority (lowest value) jobs first.

## Dead Letter Queue

When jobs exhaust all retry attempts, they are moved to the dead letter queue:

```rust
// Inspect failed jobs
let dead_jobs = queue.drain_dlq("my_queue", 100).await?;
for job in dead_jobs {
    println!(
        "Job {} failed after {} attempts: {:?}",
        job.original_job_id,
        job.attempts,
        job.error_message
    );
}

// Requeue a failed job for retry
let new_job_id = queue.requeue_dlq(dead_job.id).await?;

// Or delete it permanently
queue.delete_dlq_job(dead_job.id).await?;

// Get count of failed jobs
let count = queue.dlq_count("my_queue").await?;
```

### Error Handling and Retries

```rust
use tokio_pgqueue::{PgQueue, QueueConfig, EnqueueOptions};

async fn process_with_retry(queue: &PgQueue) -> Result<(), Box<dyn std::error::Error>> {
    let worker_id = "worker-1";

    if let Some(job) = queue.claim("my_queue", worker_id).await? {
        // Send heartbeats periodically
        let queue_clone = queue.clone();
        let job_id = job.id;
        let worker_id_clone = worker_id.to_string();
        let heartbeat_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let _ = queue_clone.heartbeat(job_id, &worker_id_clone).await;
            }
        });

        // Process the job
        match process_job(&job).await {
            Ok(()) => {
                queue.complete(job.id, worker_id).await?;
            }
            Err(e) => {
                // Mark as failed - will be retried with exponential backoff
                // After max attempts, moves to DLQ
                queue.fail(job.id, worker_id, &format!("{:?}", e)).await?;
            }
        }

        heartbeat_handle.abort();
    }

    Ok(())
}

// Configure max attempts per job
let options = EnqueueOptions::new()
    .max_attempts(5);  // Job will be retried up to 5 times before DLQ
let job_id = queue.enqueue_with_options("my_queue", "task", serde_json::json!({}), options).await?;
```

### Reclaiming Orphaned Jobs

If a worker crashes, jobs it was processing become "orphaned". You can reclaim them:

```rust
use tokio_pgqueue::PgQueue;
use std::time::Duration;

async fn reclaim_jobs(queue: &PgQueue) -> Result<(), Box<dyn std::error::Error>> {
    // Reclaim jobs that haven't had a heartbeat in 5 minutes
    let orphaned = queue
        .reclaim_orphans("my_queue", Duration::from_secs(300))
        .await?;

    println!("Reclaimed {} orphaned jobs", orphaned.len());

    Ok(())
}
```

The `QueueWorker` automatically reclaims orphans on startup if `auto_reclaim` is enabled (default: `true`).

## Configuration

### QueueConfig

```rust
use tokio_pgqueue::QueueConfig;
use std::time::Duration;

let config = QueueConfig {
    heartbeat_timeout: Duration::from_secs(300), // 5 minutes
    poll_interval: Duration::from_millis(500),
    heartbeat_interval: Duration::from_secs(30),
};
```

### WorkerConfig

```rust
use tokio_pgqueue::WorkerBuilder;
use std::time::Duration;

let worker = WorkerBuilder::new("my_queue", "worker-1")
    .poll_interval(Duration::from_secs(1))
    .heartbeat_interval(Duration::from_secs(30))
    .auto_reclaim(true)
    .reclaim_timeout(Duration::from_secs(300))
    // ... set handler and queue
    .build()?;
```

### EnqueueOptions

```rust
use tokio_pgqueue::EnqueueOptions;
use chrono::{Utc, Duration};

let options = EnqueueOptions::new()
    .priority(50)                              // Higher priority (lower = higher)
    .run_at(Utc::now() + Duration::hours(1))   // Schedule for later
    .max_attempts(5);                          // Custom retry limit
```

## Database Schema

The crate automatically creates the following schema when you call `PgQueue::new()`:

```sql
CREATE TYPE job_status AS ENUM ('pending', 'running', 'completed', 'failed');

CREATE TABLE job_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    queue_name TEXT NOT NULL,
    job_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    status job_status NOT NULL DEFAULT 'pending',
    priority SMALLINT NOT NULL DEFAULT 100,
    attempts INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ,
    worker_id TEXT,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE dlq_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    original_job_id UUID NOT NULL,
    queue_name TEXT NOT NULL,
    job_type TEXT NOT NULL,
    payload JSONB,
    attempts INT NOT NULL,
    max_attempts INT NOT NULL,
    error_message TEXT,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_job_queue_pending ON job_queue(queue_name, status, priority, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX idx_job_queue_running ON job_queue(queue_name, status, last_heartbeat_at)
    WHERE status = 'running';

CREATE INDEX idx_dlq_jobs_queue ON dlq_jobs(queue_name, failed_at);
```

## Job Lifecycle

1. **Pending** - Job is enqueued and waiting to be claimed
2. **Running** - Job is claimed by a worker and being processed
3. **Completed** - Job finished successfully
4. **Failed → DLQ** - Job failed and exceeded max retry attempts, moved to dead letter queue

When a job fails, it is automatically re-queued with exponential backoff (2^attempts seconds, capped at ~17 minutes) until `max_attempts` is reached (default: 3), after which it moves to the DLQ.

## Requirements

- Rust 1.80 or later
- PostgreSQL 12 or later
- The `gen_random_uuid()` function (available in PostgreSQL 13+, or enable the `pgcrypto` extension)

## License

MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.
