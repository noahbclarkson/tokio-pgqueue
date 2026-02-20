//! # tokio-pgqueue
//!
//! A Postgres-backed job queue for Rust with heartbeat-based job claims, crash recovery,
//! exponential backoff for retries, scheduled jobs, job priority, and dead letter queue support.
//!
//! ## Features
//!
//! - **Heartbeat-based job claims**: Workers claim jobs and periodically send heartbeats
//! - **Crash recovery**: Orphaned jobs (where the worker died) can be reclaimed
//! - **Exponential backoff**: Failed jobs are automatically retried with increasing delays
//! - **Scheduled jobs**: Jobs can be scheduled for future execution
//! - **Job priority**: Jobs can have priorities (lower = higher priority)
//! - **Dead letter queue**: Failed jobs are moved to DLQ for inspection and requeue
//! - **Tokio-native**: Built on tokio and sqlx for full async support
//!
//! ## Example
//!
//! ```no_run
//! use tokio_pgqueue::{PgQueue, QueueConfig, EnqueueOptions};
//! use sqlx::PgPool;
//! use chrono::{Utc, Duration};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a database connection pool
//! let pool = PgPool::connect("postgres://user:pass@localhost/db").await?;
//!
//! // Create the queue with default configuration
//! let config = QueueConfig::default();
//! let queue = PgQueue::new(pool, config).await?;
//!
//! // Enqueue a job
//! let job_id = queue.enqueue("my_queue", "process_image", serde_json::json!({"path": "/tmp/img.jpg"})).await?;
//!
//! // Schedule a job for future execution
//! let run_at = Utc::now() + Duration::hours(1);
//! let scheduled_id = queue.enqueue_at("my_queue", "cleanup", serde_json::json!({}), run_at).await?;
//!
//! // Enqueue with options (priority, scheduling, max attempts)
//! let options = EnqueueOptions::new()
//!     .priority(50)  // Higher priority (lower = higher)
//!     .max_attempts(5);
//! let job_id = queue.enqueue_with_options("my_queue", "important", serde_json::json!({}), options).await?;
//!
//! // Claim a job for processing
//! if let Some(job) = queue.claim("my_queue", "worker-1").await? {
//!     // Send periodic heartbeats while processing
//!     queue.heartbeat(job.id, "worker-1").await?;
//!
//!     // Process the job...
//!
//!     // Mark as completed
//!     queue.complete(job.id, "worker-1").await?;
//! }
//! # Ok(())
//! # }
//! ```

mod error;
mod types;
mod worker;

pub use error::{QueueError, Result};
pub use types::{DeadJob, DlqConfig, EnqueueOptions, Job, JobId, JobStatus, QueueConfig};
pub use worker::{QueueWorker, WorkerBuilder};

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use std::time::Duration as StdDuration;
use tracing::{debug, info, warn};

/// The main queue interface for managing Postgres-backed jobs.
#[derive(Clone)]
pub struct PgQueue {
    pool: PgPool,
    config: QueueConfig,
}

impl PgQueue {
    /// Create a new PgQueue instance.
    ///
    /// This will automatically run any pending database migrations.
    ///
    /// # Arguments
    ///
    /// * `pool` - A SQLx Postgres connection pool
    /// * `config` - Configuration for the queue
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tokio_pgqueue::{PgQueue, QueueConfig};
    /// # use sqlx::PgPool;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let pool = PgPool::connect("postgres://localhost/db").await?;
    /// let config = QueueConfig::default();
    /// let queue = PgQueue::new(pool, config).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(pool: PgPool, config: QueueConfig) -> Result<Self> {
        // Run migrations
        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(PgQueue { pool, config })
    }

    /// Get a reference to the database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get a reference to the queue configuration.
    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    /// Enqueue a new job.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to enqueue to
    /// * `job_type` - A string identifier for the type of job (useful for workers to know how to process)
    /// * `payload` - Arbitrary JSON payload for the job
    ///
    /// # Returns
    ///
    /// The ID of the newly created job.
    pub async fn enqueue(
        &self,
        queue_name: &str,
        job_type: &str,
        payload: serde_json::Value,
    ) -> Result<JobId> {
        self.enqueue_with_options(queue_name, job_type, payload, EnqueueOptions::default())
            .await
    }

    /// Enqueue a new job with custom options.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to enqueue to
    /// * `job_type` - A string identifier for the type of job
    /// * `payload` - Arbitrary JSON payload for the job
    /// * `options` - Custom enqueue options (priority, scheduling, max attempts)
    ///
    /// # Returns
    ///
    /// The ID of the newly created job.
    pub async fn enqueue_with_options(
        &self,
        queue_name: &str,
        job_type: &str,
        payload: serde_json::Value,
        options: EnqueueOptions,
    ) -> Result<JobId> {
        let scheduled_at = options.run_at.unwrap_or_else(Utc::now);
        let max_attempts = options.max_attempts.unwrap_or(3) as i32;
        let priority = options.priority as i16;

        let row = sqlx::query!(
            r#"
            INSERT INTO job_queue (queue_name, job_type, payload, scheduled_at, max_attempts, priority)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
            queue_name,
            job_type,
            payload,
            scheduled_at,
            max_attempts,
            priority
        )
        .fetch_one(&self.pool)
        .await?;

        debug!(
            "Enqueued job {} (type: {}, queue: {}, priority: {}, scheduled_at: {})",
            row.id, job_type, queue_name, options.priority, scheduled_at
        );
        Ok(row.id)
    }

    /// Enqueue a job to run at a specific future time.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to enqueue to
    /// * `job_type` - A string identifier for the type of job
    /// * `payload` - Arbitrary JSON payload for the job
    /// * `run_at` - The time when the job should become available for processing
    ///
    /// # Returns
    ///
    /// The ID of the newly created job.
    pub async fn enqueue_at(
        &self,
        queue_name: &str,
        job_type: &str,
        payload: serde_json::Value,
        run_at: DateTime<Utc>,
    ) -> Result<JobId> {
        let options = EnqueueOptions::new().run_at(run_at);
        self.enqueue_with_options(queue_name, job_type, payload, options)
            .await
    }

    /// Enqueue a job to run after a delay.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to enqueue to
    /// * `job_type` - A string identifier for the type of job
    /// * `payload` - Arbitrary JSON payload for the job
    /// * `delay` - How long to wait before the job becomes available
    ///
    /// # Returns
    ///
    /// The ID of the newly created job.
    pub async fn enqueue_after(
        &self,
        queue_name: &str,
        job_type: &str,
        payload: serde_json::Value,
        delay: StdDuration,
    ) -> Result<JobId> {
        let run_at =
            Utc::now() + Duration::from_std(delay).unwrap_or_else(|_| Duration::seconds(0));
        self.enqueue_at(queue_name, job_type, payload, run_at).await
    }

    /// Claim the next available job from a queue.
    ///
    /// This uses `SELECT FOR UPDATE SKIP LOCKED` to safely claim a job without
    /// blocking on jobs already claimed by other workers.
    ///
    /// Jobs are claimed in priority order (lower priority value = higher priority),
    /// then by scheduled_at time.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to claim from
    /// * `worker_id` - A unique identifier for this worker instance
    ///
    /// # Returns
    ///
    /// `Some(job)` if a job was claimed, `None` if the queue is empty.
    pub async fn claim(&self, queue_name: &str, worker_id: &str) -> Result<Option<Job>> {
        let now = Utc::now();

        let result = sqlx::query!(
            r#"
            UPDATE job_queue
            SET
                status = 'running',
                worker_id = $1,
                started_at = $2,
                last_heartbeat_at = $2
            WHERE id = (
                SELECT id FROM job_queue
                WHERE queue_name = $3
                    AND status = 'pending'
                    AND scheduled_at <= $2
                ORDER BY priority ASC, scheduled_at ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            RETURNING
                id, queue_name, job_type, payload, status as "status: JobStatus",
                priority, attempts, max_attempts, scheduled_at, started_at, completed_at,
                last_heartbeat_at, worker_id, error_message, created_at, updated_at
            "#,
            worker_id,
            now,
            queue_name
        )
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(row) => {
                let job = Job {
                    id: row.id,
                    queue_name: row.queue_name,
                    job_type: row.job_type,
                    payload: row.payload,
                    status: row.status,
                    priority: row.priority as u8,
                    attempts: row.attempts as u32,
                    max_attempts: row.max_attempts as u32,
                    scheduled_at: row.scheduled_at,
                    started_at: row.started_at,
                    completed_at: row.completed_at,
                    last_heartbeat_at: row.last_heartbeat_at,
                    worker_id: row.worker_id,
                    error_message: row.error_message,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                };
                info!(
                    "Worker {} claimed job {} from queue {} (priority {})",
                    worker_id, job.id, queue_name, job.priority
                );
                Ok(Some(job))
            }
            None => {
                debug!("No pending jobs available in queue {}", queue_name);
                Ok(None)
            }
        }
    }

    /// Send a heartbeat for a claimed job.
    ///
    /// Workers should call this periodically (e.g., every 30 seconds) while processing
    /// a job to indicate they're still alive.
    ///
    /// # Arguments
    ///
    /// * `job_id` - The ID of the job being processed
    /// * `worker_id` - The worker ID that claimed the job
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or wasn't claimed by this worker.
    pub async fn heartbeat(&self, job_id: JobId, worker_id: &str) -> Result<()> {
        let now = Utc::now();

        let result = sqlx::query!(
            r#"
            UPDATE job_queue
            SET last_heartbeat_at = $1
            WHERE id = $2 AND worker_id = $3 AND status = 'running'
            RETURNING id
            "#,
            now,
            job_id,
            worker_id
        )
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(_) => Ok(()),
            None => {
                // Job might not exist or not owned by this worker
                let job = sqlx::query!("SELECT id, worker_id FROM job_queue WHERE id = $1", job_id)
                    .fetch_optional(&self.pool)
                    .await?;

                match job {
                    Some(row) => {
                        if let Some(claimed_by) = row.worker_id {
                            if claimed_by != worker_id {
                                return Err(QueueError::OwnershipViolation {
                                    job_id,
                                    claimed_by,
                                    worker_id: worker_id.to_string(),
                                });
                            }
                        }
                        Err(QueueError::InvalidJobState(job_id))
                    }
                    None => Err(QueueError::JobNotFound(job_id)),
                }
            }
        }
    }

    /// Mark a job as successfully completed.
    ///
    /// # Arguments
    ///
    /// * `job_id` - The ID of the job to complete
    /// * `worker_id` - The worker ID that claimed the job
    ///
    /// # Errors
    ///
    /// Returns an error if the job wasn't claimed by this worker.
    pub async fn complete(&self, job_id: JobId, worker_id: &str) -> Result<()> {
        let now = Utc::now();

        let result = sqlx::query!(
            r#"
            UPDATE job_queue
            SET
                status = 'completed',
                completed_at = $1,
                worker_id = NULL
            WHERE id = $2 AND worker_id = $3 AND status = 'running'
            RETURNING id
            "#,
            now,
            job_id,
            worker_id
        )
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(_) => {
                info!("Job {} completed by worker {}", job_id, worker_id);
                Ok(())
            }
            None => {
                // Check ownership
                let job = sqlx::query!(
                    "SELECT id, worker_id, status as \"status: JobStatus\" FROM job_queue WHERE id = $1",
                    job_id
                )
                .fetch_optional(&self.pool)
                .await?;

                match job {
                    Some(row) => {
                        if let Some(claimed_by) = row.worker_id {
                            if claimed_by != worker_id {
                                return Err(QueueError::OwnershipViolation {
                                    job_id,
                                    claimed_by,
                                    worker_id: worker_id.to_string(),
                                });
                            }
                        }
                        Err(QueueError::InvalidJobState(job_id))
                    }
                    None => Err(QueueError::JobNotFound(job_id)),
                }
            }
        }
    }

    /// Mark a job as failed.
    ///
    /// If the job has remaining attempts, it will be re-queued with an exponential backoff
    /// delay. If it has exceeded max attempts, it will be moved to the dead letter queue.
    ///
    /// # Arguments
    ///
    /// * `job_id` - The ID of the job that failed
    /// * `worker_id` - The worker ID that was processing the job
    /// * `error_message` - A description of what went wrong
    ///
    /// # Errors
    ///
    /// Returns an error if the job wasn't claimed by this worker.
    pub async fn fail(&self, job_id: JobId, worker_id: &str, error_message: &str) -> Result<()> {
        let now = Utc::now();

        // First, get the job details
        let job = sqlx::query!(
            r#"
            SELECT
                id, queue_name, job_type, payload, priority,
                attempts, max_attempts, scheduled_at,
                worker_id, status as "status: JobStatus"
            FROM job_queue
            WHERE id = $1
            "#,
            job_id
        )
        .fetch_optional(&self.pool)
        .await?;

        match job {
            Some(row) => {
                // Verify ownership
                if row.worker_id.as_deref() != Some(worker_id) {
                    return Err(QueueError::OwnershipViolation {
                        job_id,
                        claimed_by: row.worker_id.unwrap_or_else(|| "none".to_string()),
                        worker_id: worker_id.to_string(),
                    });
                }

                if row.status != JobStatus::Running {
                    return Err(QueueError::InvalidJobState(job_id));
                }

                let new_attempts = row.attempts + 1;

                if new_attempts > row.max_attempts {
                    // Move to dead letter queue
                    sqlx::query!(
                        r#"
                        INSERT INTO dlq_jobs (original_job_id, queue_name, job_type, payload, attempts, max_attempts, error_message)
                        VALUES ($1, $2, $3, $4, $5, $6, $7)
                        "#,
                        job_id,
                        row.queue_name,
                        row.job_type,
                        row.payload,
                        new_attempts,
                        row.max_attempts,
                        error_message
                    )
                    .execute(&self.pool)
                    .await?;

                    // Delete from main queue
                    sqlx::query!("DELETE FROM job_queue WHERE id = $1", job_id)
                        .execute(&self.pool)
                        .await?;

                    warn!(
                        "Job {} moved to DLQ after {} attempts: {}",
                        job_id, row.max_attempts, error_message
                    );
                    Ok(())
                } else {
                    // Re-queue with exponential backoff
                    // Backoff: 2^attempts seconds, capped at 1 hour
                    let backoff_seconds = 2u64.pow((new_attempts as u32).min(10)); // Max 2^10 = 1024 seconds ~ 17 min
                    let backoff = Duration::seconds(backoff_seconds as i64);
                    let scheduled_at = now + backoff;

                    sqlx::query!(
                        r#"
                        UPDATE job_queue
                        SET
                            status = 'pending',
                            attempts = $1,
                            scheduled_at = $2,
                            started_at = NULL,
                            last_heartbeat_at = NULL,
                            worker_id = NULL,
                            error_message = $3
                        WHERE id = $4
                        "#,
                        new_attempts as i32,
                        scheduled_at,
                        error_message,
                        job_id
                    )
                    .execute(&self.pool)
                    .await?;

                    info!(
                        "Job {} re-queued (attempt {}/{}) in {:?}: {}",
                        job_id, new_attempts, row.max_attempts, backoff, error_message
                    );
                    Ok(())
                }
            }
            None => Err(QueueError::JobNotFound(job_id)),
        }
    }

    /// Reclaim orphaned jobs.
    ///
    /// Orphaned jobs are jobs that are in "running" state but haven't received a heartbeat
    /// within the timeout period. This can happen when a worker crashes without completing
    /// or failing the job.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to check
    /// * `timeout` - How long a job can be without a heartbeat before being considered orphaned
    ///
    /// # Returns
    ///
    /// A list of orphaned jobs that were reclaimed.
    pub async fn reclaim_orphans(
        &self,
        queue_name: &str,
        timeout: StdDuration,
    ) -> Result<Vec<Job>> {
        let timeout_chrono = Duration::seconds(timeout.as_secs() as i64);
        let cutoff = Utc::now() - timeout_chrono;

        let rows = sqlx::query!(
            r#"
            UPDATE job_queue
            SET
                status = 'pending',
                worker_id = NULL,
                started_at = NULL,
                last_heartbeat_at = NULL
            WHERE
                queue_name = $1
                AND status = 'running'
                AND last_heartbeat_at < $2
            RETURNING
                id, queue_name, job_type, payload, status as "status: JobStatus",
                priority, attempts, max_attempts, scheduled_at, started_at, completed_at,
                last_heartbeat_at, worker_id, error_message, created_at, updated_at
            "#,
            queue_name,
            cutoff
        )
        .fetch_all(&self.pool)
        .await?;

        let jobs: Vec<Job> = rows
            .into_iter()
            .map(|row| Job {
                id: row.id,
                queue_name: row.queue_name,
                job_type: row.job_type,
                payload: row.payload,
                status: row.status,
                priority: row.priority as u8,
                attempts: row.attempts as u32,
                max_attempts: row.max_attempts as u32,
                scheduled_at: row.scheduled_at,
                started_at: row.started_at,
                completed_at: row.completed_at,
                last_heartbeat_at: row.last_heartbeat_at,
                worker_id: row.worker_id,
                error_message: row.error_message,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
            .collect();

        if !jobs.is_empty() {
            info!(
                "Reclaimed {} orphaned jobs from queue {}",
                jobs.len(),
                queue_name
            );
        }

        Ok(jobs)
    }

    /// Get a job by ID.
    pub async fn get_job(&self, job_id: JobId) -> Result<Option<Job>> {
        let row = sqlx::query!(
            r#"
            SELECT
                id, queue_name, job_type, payload, status as "status: JobStatus",
                priority, attempts, max_attempts, scheduled_at, started_at, completed_at,
                last_heartbeat_at, worker_id, error_message, created_at, updated_at
            FROM job_queue
            WHERE id = $1
            "#,
            job_id
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Job {
                id: row.id,
                queue_name: row.queue_name,
                job_type: row.job_type,
                payload: row.payload,
                status: row.status,
                priority: row.priority as u8,
                attempts: row.attempts as u32,
                max_attempts: row.max_attempts as u32,
                scheduled_at: row.scheduled_at,
                started_at: row.started_at,
                completed_at: row.completed_at,
                last_heartbeat_at: row.last_heartbeat_at,
                worker_id: row.worker_id,
                error_message: row.error_message,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })),
            None => Ok(None),
        }
    }

    // ========================================
    // Dead Letter Queue Operations
    // ========================================

    /// Drain jobs from the dead letter queue.
    ///
    /// Returns up to `limit` dead jobs from the specified queue, ordered by failure time.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to drain from
    /// * `limit` - Maximum number of jobs to return
    ///
    /// # Returns
    ///
    /// A list of dead jobs from the DLQ.
    pub async fn drain_dlq(&self, queue_name: &str, limit: u32) -> Result<Vec<DeadJob>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                id, original_job_id, queue_name, job_type, payload,
                attempts, max_attempts, error_message, failed_at, created_at
            FROM dlq_jobs
            WHERE queue_name = $1
            ORDER BY failed_at ASC
            LIMIT $2
            "#,
            queue_name,
            limit as i64
        )
        .fetch_all(&self.pool)
        .await?;

        let dead_jobs: Vec<DeadJob> = rows
            .into_iter()
            .map(|row| DeadJob {
                id: row.id,
                original_job_id: row.original_job_id,
                queue_name: row.queue_name,
                job_type: row.job_type,
                payload: row.payload,
                attempts: row.attempts as u32,
                max_attempts: row.max_attempts as u32,
                error_message: row.error_message,
                failed_at: row.failed_at,
                created_at: row.created_at,
            })
            .collect();

        debug!(
            "Drained {} jobs from DLQ queue {}",
            dead_jobs.len(),
            queue_name
        );
        Ok(dead_jobs)
    }

    /// Requeue a job from the dead letter queue.
    ///
    /// This moves a job from the DLQ back to the main queue for reprocessing.
    ///
    /// # Arguments
    ///
    /// * `dlq_job_id` - The ID of the job in the DLQ (not the original job ID)
    ///
    /// # Returns
    ///
    /// The ID of the newly created job in the main queue.
    pub async fn requeue_dlq(&self, dlq_job_id: uuid::Uuid) -> Result<JobId> {
        // Get the DLQ job
        let dlq_job = sqlx::query!(
            r#"
            SELECT
                id, original_job_id, queue_name, job_type, payload,
                attempts, max_attempts, error_message
            FROM dlq_jobs
            WHERE id = $1
            "#,
            dlq_job_id
        )
        .fetch_optional(&self.pool)
        .await?;

        match dlq_job {
            Some(row) => {
                // Insert back into main queue with reset attempts
                let new_job = sqlx::query!(
                    r#"
                    INSERT INTO job_queue (queue_name, job_type, payload, max_attempts)
                    VALUES ($1, $2, $3, $4)
                    RETURNING id
                    "#,
                    row.queue_name,
                    row.job_type,
                    row.payload,
                    row.max_attempts
                )
                .fetch_one(&self.pool)
                .await?;

                // Delete from DLQ
                sqlx::query!("DELETE FROM dlq_jobs WHERE id = $1", dlq_job_id)
                    .execute(&self.pool)
                    .await?;

                info!(
                    "Requeued DLQ job {} as new job {} in queue {}",
                    dlq_job_id, new_job.id, row.queue_name
                );

                Ok(new_job.id)
            }
            None => Err(QueueError::DlqJobNotFound(dlq_job_id)),
        }
    }

    /// Get a count of jobs in the DLQ for a queue.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to count
    ///
    /// # Returns
    ///
    /// The number of jobs in the DLQ for this queue.
    pub async fn dlq_count(&self, queue_name: &str) -> Result<u64> {
        let row = sqlx::query!(
            "SELECT COUNT(*) as count FROM dlq_jobs WHERE queue_name = $1",
            queue_name
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.count.unwrap_or(0) as u64)
    }

    /// Delete a job from the DLQ permanently.
    ///
    /// # Arguments
    ///
    /// * `dlq_job_id` - The ID of the job in the DLQ to delete
    ///
    /// # Returns
    ///
    /// `true` if the job was deleted, `false` if it wasn't found.
    pub async fn delete_dlq_job(&self, dlq_job_id: uuid::Uuid) -> Result<bool> {
        let result = sqlx::query!("DELETE FROM dlq_jobs WHERE id = $1", dlq_job_id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}
