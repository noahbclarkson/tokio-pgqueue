// Integration tests for tokio-pgqueue
//
// These tests require a running PostgreSQL instance.
// Set the DATABASE_URL environment variable to connect.
//
// Example:
//   export DATABASE_URL="postgres://postgres:password@localhost/tokio_pgqueue_test"
//   cargo test

use sqlx::PgPool;
use tokio_pgqueue::{Job, JobStatus, PgQueue, QueueConfig, QueueWorker, WorkerBuilder};
use uuid::Uuid;

// Helper to get a test database URL
fn test_db_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgres@localhost/tokio_pgqueue_test".to_string()
    })
}

#[tokio::test]
async fn test_create_queue() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Clean up any existing tables
    sqlx::query("DROP TABLE IF EXISTS job_queue CASCADE")
        .execute(queue.pool())
        .await?;

    // Recreate queue to test migrations
    let queue = PgQueue::new(queue.pool().clone(), config).await?;

    assert_eq!(queue.config().heartbeat_timeout.as_secs(), 300);
    Ok(())
}

#[tokio::test]
async fn test_enqueue_and_get_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    let payload = serde_json::json!({"test": "data"});
    let job_id = queue.enqueue("test_queue", "test_job", payload.clone()).await?;

    let job = queue.get_job(job_id).await?.unwrap();

    assert_eq!(job.id, job_id);
    assert_eq!(job.queue_name, "test_queue");
    assert_eq!(job.job_type, "test_job");
    assert_eq!(job.payload, payload);
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.attempts, 0);
    assert_eq!(job.max_attempts, 3);

    Ok(())
}

#[tokio::test]
async fn test_claim_and_complete_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Enqueue a job
    let job_id = queue
        .enqueue("test_queue", "test_job", serde_json::json!({"test": "data"}))
        .await?;

    // Claim the job
    let job = queue.claim("test_queue", "worker-1").await?.unwrap();

    assert_eq!(job.id, job_id);
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.worker_id, Some("worker-1".to_string()));
    assert!(job.started_at.is_some());
    assert!(job.last_heartbeat_at.is_some());

    // Complete the job
    queue.complete(job_id, "worker-1").await?;

    // Verify completion
    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert!(job.completed_at.is_some());
    assert!(job.worker_id.is_none());

    Ok(())
}

#[tokio::test]
async fn test_heartbeat() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Enqueue and claim a job
    let job_id = queue
        .enqueue("test_queue", "test_job", serde_json::json!({}))
        .await?;
    let job = queue.claim("test_queue", "worker-1").await?.unwrap();

    let initial_heartbeat = job.last_heartbeat_at.unwrap();

    // Wait a bit and send heartbeat
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    queue.heartbeat(job_id, "worker-1").await?;

    // Verify heartbeat was updated
    let job = queue.get_job(job_id).await?.unwrap();
    assert!(job.last_heartbeat_at.unwrap() > initial_heartbeat);

    Ok(())
}

#[tokio::test]
async fn test_fail_and_retry() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgPool::new(pool, config).await?;

    // Enqueue and claim a job
    let job_id = queue
        .enqueue("test_queue", "test_job", serde_json::json!({}))
        .await?;
    let job = queue.claim("test_queue", "worker-1").await?.unwrap();

    assert_eq!(job.attempts, 0);

    // Fail the job
    queue
        .fail(job_id, "worker-1", "Something went wrong")
        .await?;

    // Job should be re-queued with attempt = 1
    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.attempts, 1);
    assert!(job.started_at.is_none());
    assert!(job.worker_id.is_none());

    // Claim again and fail a second time
    queue.claim("test_queue", "worker-1").await?;
    queue
        .fail(job_id, "worker-1", "Failed again")
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.attempts, 2);

    // Claim a third time and fail - should be permanently failed (max_attempts = 3)
    queue.claim("test_queue", "worker-1").await?;
    queue
        .fail(job_id, "worker-1", "Final failure")
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(job.attempts, 3);
    assert!(job.completed_at.is_some());

    Ok(())
}

#[tokio::test]
async fn test_reclaim_orphans() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgPool::new(pool, config).await?;

    // Enqueue and claim a job
    let job_id = queue
        .enqueue("test_queue", "test_job", serde_json::json!({}))
        .await?;
    queue.claim("test_queue", "worker-1").await?;

    // Manually update last_heartbeat_at to make it look orphaned
    sqlx::query!(
        "UPDATE job_queue SET last_heartbeat_at = NOW() - INTERVAL '10 minutes' WHERE id = $1",
        job_id
    )
    .execute(queue.pool())
    .await?;

    // Reclaim orphans
    let reclaimed = queue
        .reclaim_orphans("test_queue", std::time::Duration::from_secs(300))
        .await?;

    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, job_id);
    assert_eq!(reclaimed[0].status, JobStatus::Pending);

    // Verify job is back to pending state
    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Pending);
    assert!(job.worker_id.is_none());

    Ok(())
}

#[tokio::test]
async fn test_queue_isolation() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgPool::new(pool, config).await?;

    // Enqueue jobs in different queues
    let job1 = queue
        .enqueue("queue_a", "job", serde_json::json!({}))
        .await?;
    let job2 = queue
        .enqueue("queue_b", "job", serde_json::json!({}))
        .await?;

    // Claim from queue_a should only get job1
    let claimed = queue.claim("queue_a", "worker-1").await?.unwrap();
    assert_eq!(claimed.id, job1);

    // Claim from queue_b should only get job2
    let claimed = queue.claim("queue_b", "worker-1").await?.unwrap();
    assert_eq!(claimed.id, job2);

    Ok(())
}

#[tokio::test]
async fn test_ownership_violation() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgPool::new(pool, config).await?;

    // Enqueue and claim a job with worker-1
    let job_id = queue
        .enqueue("test_queue", "test_job", serde_json::json!({}))
        .await?;
    queue.claim("test_queue", "worker-1").await?;

    // Try to heartbeat with a different worker should fail
    let result = queue.heartbeat(job_id, "worker-2").await;
    assert!(result.is_err());

    // Try to complete with a different worker should fail
    let result = queue.complete(job_id, "worker-2").await;
    assert!(result.is_err());

    // But worker-1 can complete it
    queue.complete(job_id, "worker-1").await?;

    Ok(())
}

#[tokio::test]
async fn test_worker_basic() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgPool::new(pool, config).await?;

    // Enqueue a job
    let job_id = queue
        .enqueue("worker_test", "test_job", serde_json::json!({"value": 42}))
        .await?;

    // Create a worker that will process exactly one job then stop
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let processed = Arc::new(AtomicBool::new(false));
    let processed_clone = processed.clone();

    let worker = WorkerBuilder::new("worker_test", "test-worker")
        .queue(queue.clone())
        .poll_interval(std::time::Duration::from_millis(100))
        .handler_simple(move |job| {
            let processed = processed_clone.clone();
            async move {
                assert_eq!(job.id, job_id);
                assert_eq!(job.payload["value"], 42);
                processed.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
        .build()?;

    // Run worker in background
    let worker_handle = tokio::spawn(async move {
        let _ = worker.run().await;
    });

    // Wait for job to be processed
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Verify job was processed
    assert!(processed.load(Ordering::SeqCst));

    // Clean up
    worker_handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_skip_locked() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgPool::new(pool, config).await?;

    // Enqueue 3 jobs
    let job1 = queue
        .enqueue("test_queue", "job", serde_json::json!({"n": 1}))
        .await?;
    let _job2 = queue
        .enqueue("test_queue", "job", serde_json::json!({"n": 2}))
        .await?;
    let _job3 = queue
        .enqueue("test_queue", "job", serde_json::json!({"n": 3}))
        .await?;

    // Worker 1 claims a job
    let claimed1 = queue.claim("test_queue", "worker-1").await?.unwrap();
    assert_eq!(claimed1.id, job1);

    // Worker 2 should be able to claim a different job (not blocked by worker 1's lock)
    let claimed2 = queue.claim("test_queue", "worker-2").await?.unwrap();
    assert_ne!(claimed2.id, job1);
    assert_eq!(claimed2.worker_id, Some("worker-2".to_string()));

    // Worker 1 cannot complete worker 2's job
    let result = queue.complete(claimed2.id, "worker-1").await;
    assert!(result.is_err());

    Ok(())
}
