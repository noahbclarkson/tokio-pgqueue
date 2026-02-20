//! # tokio-pgqueue
//!
//! A Postgres-backed job queue for Rust with heartbeat-based job claims, crash recovery,
//! and exponential backoff for retries.
//!
//! ## Features
//!
//! - **Heartbeat-based job claims**: Workers claim jobs and periodically send heartbeats
//! - **Crash recovery**: Orphaned jobs (where the worker died) can be reclaimed
//! - **Exponential backoff**: Failed jobs are automatically retried with increasing delays
//! - **Tokio-native**: Built on tokio and sqlx for full async support
//!
//! ## Example
//!
//! ```no_run
//! use tokio_pgqueue::{PgQueue, QueueConfig};
//! use sqlx::PgPool;
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
pub use types::{Job, JobId, JobStatus, QueueConfig};
pub use worker::{QueueWorker, WorkerBuilder};

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Row};
use std::time::Duration as StdDuration;
use tracing::{debug, error, info, warn};

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
        let row = sqlx::query!(
            r#"
            INSERT INTO job_queue (queue_name, job_type, payload)
            VALUES ($1, $2, $3)
            RETURNING id
            "#,
            queue_name,
            job_type,
            payload
        )
        .fetch_one(&self.pool)
        .await?;

        debug!("Enqueued job {} (type: {}, queue: {})", row.id, job_type, queue_name);
        Ok(row.id)
    }

    /// Claim the next available job from a queue.
    ///
    /// This uses `SELECT FOR UPDATE SKIP LOCKED` to safely claim a job without
    /// blocking on jobs already claimed by other workers.
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
                ORDER BY scheduled_at ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            RETURNING
                id, queue_name, job_type, payload, status as "status: JobStatus",
                attempts, max_attempts, scheduled_at, started_at, completed_at,
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
                    "Worker {} claimed job {} from queue {}",
                    worker_id, job.id, queue_name
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
                let job = sqlx::query!(
                    "SELECT id, worker_id FROM job_queue WHERE id = $1",
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
    /// delay. If it has exceeded max attempts, it will remain in the failed state.
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
                id, attempts, max_attempts, scheduled_at,
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
                    // No more retries, mark as permanently failed
                    sqlx::query!(
                        r#"
                        UPDATE job_queue
                        SET
                            status = 'failed',
                            error_message = $1,
                            completed_at = $2,
                            worker_id = NULL
                        WHERE id = $3
                        "#,
                        error_message,
                        now,
                        job_id
                    )
                    .execute(&self.pool)
                    .await?;

                    warn!(
                        "Job {} permanently failed after {} attempts: {}",
                        job_id, row.max_attempts, error_message
                    );
                    Ok(())
                } else {
                    // Re-queue with exponential backoff
                    // Backoff: 2^attempts seconds, capped at 1 hour
                    let backoff_seconds = 2u64.pow(new_attempts.min(10)); // Max 2^10 = 1024 seconds ~ 17 min
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
                attempts, max_attempts, scheduled_at, started_at, completed_at,
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
            info!("Reclaimed {} orphaned jobs from queue {}", jobs.len(), queue_name);
        }

        Ok(jobs)
    }

    /// Get a job by ID.
    pub async fn get_job(&self, job_id: JobId) -> Result<Option<Job>> {
        let row = sqlx::query!(
            r#"
            SELECT
                id, queue_name, job_type, payload, status as "status: JobStatus",
                attempts, max_attempts, scheduled_at, started_at, completed_at,
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
}
