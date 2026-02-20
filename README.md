# tokio-pgqueue

A Postgres-backed job queue for Rust with heartbeat-based job claims, crash recovery, and exponential backoff for retries.

## Features

- **Heartbeat-based job claims** - Workers claim jobs and periodically send heartbeats to indicate they're still alive
- **Crash recovery** - Orphaned jobs (where the worker crashed) can be automatically reclaimed
- **Exponential backoff** - Failed jobs are automatically retried with increasing delays
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

### Error Handling and Retries

```rust
use tokio_pgqueue::{PgQueue, QueueConfig};

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
                queue.fail(job.id, worker_id, &format!("{:?}", e)).await?;
            }
        }

        heartbeat_handle.abort();
    }

    Ok(())
}
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

CREATE INDEX idx_job_queue_pending ON job_queue(queue_name, status, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX idx_job_queue_running ON job_queue(queue_name, status, last_heartbeat_at)
    WHERE status = 'running';
```

## Job Lifecycle

1. **Pending** - Job is enqueued and waiting to be claimed
2. **Running** - Job is claimed by a worker and being processed
3. **Completed** - Job finished successfully
4. **Failed** - Job failed and exceeded max retry attempts

When a job fails, it is automatically re-queued with exponential backoff (2^attempts seconds, capped at ~17 minutes) until `max_attempts` is reached (default: 3).

## Requirements

- Rust 1.70 or later
- PostgreSQL 12 or later
- The `gen_random_uuid()` function (available in PostgreSQL 13+, or enable the `pgcrypto` extension)

## License

MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.
