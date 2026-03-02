// Integration tests for tokio-pgqueue
//
// These tests require a running PostgreSQL instance.
// Set the DATABASE_URL environment variable to connect.
//
// Example:
//   export DATABASE_URL="postgres://postgres:password@localhost/tokio_pgqueue_test"
//   cargo test -- --ignored

use sqlx::PgPool;
use tokio_pgqueue::{BackoffStrategy, JobStatus, PgQueue, QueueConfig, WorkerBuilder};
use uuid::Uuid;

// Helper to get a test database URL
fn test_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/tokio_pgqueue_test".to_string())
}

// Helper to generate a unique queue name for test isolation
fn unique_queue_name(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::new_v4())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_create_queue() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    assert_eq!(queue.config().heartbeat_timeout.as_secs(), 300);
    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_and_get_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("test");

    let payload = serde_json::json!({"test": "data"});
    let job_id = queue
        .enqueue(&queue_name, "test_job", payload.clone())
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();

    assert_eq!(job.id, job_id);
    assert_eq!(job.queue_name, queue_name);
    assert_eq!(job.job_type, "test_job");
    assert_eq!(job.payload, payload);
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.attempts, 0);
    assert_eq!(job.max_attempts, 5); // Default is now 5

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_claim_and_complete_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("claim_complete");

    // Enqueue a job
    let job_id = queue
        .enqueue(
            &queue_name,
            "test_job",
            serde_json::json!({"test": "data"}),
        )
        .await?;

    // Claim the job
    let job = queue.claim(&queue_name, "worker-1").await?.unwrap();

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
#[ignore = "requires postgres"]
async fn test_heartbeat() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("heartbeat");

    // Enqueue and claim a job
    let job_id = queue
        .enqueue(&queue_name, "test_job", serde_json::json!({}))
        .await?;
    let job = queue.claim(&queue_name, "worker-1").await?.unwrap();

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
#[ignore = "requires postgres"]
async fn test_fail_and_retry() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    // Use no backoff so tests can claim immediately after fail
    let config = QueueConfig {
        backoff_strategy: BackoffStrategy::None,
        ..QueueConfig::default()
    };
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("fail_retry");

    use tokio_pgqueue::EnqueueOptions;

    // Enqueue with max_attempts=3 for this test
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "test_job",
            serde_json::json!({}),
            EnqueueOptions::new().max_attempts(3),
        )
        .await?;
    let job = queue.claim(&queue_name, "worker-1").await?.unwrap();

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
    let job = queue.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(job.id, job_id);
    queue.fail(job_id, "worker-1", "Failed again").await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.attempts, 2);

    // Claim a third time and fail - should move to DLQ (we use max_attempts=3 for this test)
    let job = queue.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(job.id, job_id);
    queue.fail(job_id, "worker-1", "Final failure").await?;

    // Job should no longer exist in main queue (moved to DLQ)
    let job = queue.get_job(job_id).await?;
    assert!(job.is_none(), "Job should have been moved to DLQ");

    // Verify it's in the DLQ
    let dlq_count = queue.dlq_count(&queue_name).await?;
    assert!(dlq_count > 0, "DLQ should have at least one job");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_reclaim_orphans() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("reclaim");

    // Enqueue and claim a job
    let job_id = queue
        .enqueue(&queue_name, "test_job", serde_json::json!({}))
        .await?;
    queue.claim(&queue_name, "worker-1").await?;

    // Manually update last_heartbeat_at to make it look orphaned (using dynamic query)
    sqlx::query(
        "UPDATE job_queue SET last_heartbeat_at = NOW() - INTERVAL '10 minutes' WHERE id = $1",
    )
    .bind(job_id)
    .execute(queue.pool())
    .await?;

    // Reclaim orphans
    let reclaimed = queue
        .reclaim_orphans(&queue_name, std::time::Duration::from_secs(300))
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
#[ignore = "requires postgres"]
async fn test_queue_isolation() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_a = unique_queue_name("queue_a");
    let queue_b = unique_queue_name("queue_b");

    // Enqueue jobs in different queues
    let job1 = queue
        .enqueue(&queue_a, "job", serde_json::json!({}))
        .await?;
    let job2 = queue
        .enqueue(&queue_b, "job", serde_json::json!({}))
        .await?;

    // Claim from queue_a should only get job1
    let claimed = queue.claim(&queue_a, "worker-1").await?.unwrap();
    assert_eq!(claimed.id, job1);

    // Claim from queue_b should only get job2
    let claimed = queue.claim(&queue_b, "worker-1").await?.unwrap();
    assert_eq!(claimed.id, job2);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_ownership_violation() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("ownership");

    // Enqueue and claim a job with worker-1
    let job_id = queue
        .enqueue(&queue_name, "test_job", serde_json::json!({}))
        .await?;
    queue.claim(&queue_name, "worker-1").await?;

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
#[ignore = "requires postgres"]
async fn test_worker_basic() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("worker");

    // Enqueue a job
    let job_id = queue
        .enqueue(&queue_name, "test_job", serde_json::json!({"value": 42}))
        .await?;

    // Create a worker that will process exactly one job then stop
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let processed = Arc::new(AtomicBool::new(false));
    let processed_clone = processed.clone();

    let worker = WorkerBuilder::new(&queue_name, "test-worker")
        .queue(queue.clone())
        .poll_interval(std::time::Duration::from_millis(100))
        .handler_fn(move |job| {
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
#[ignore = "requires postgres"]
async fn test_skip_locked() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("skip_locked");

    // Enqueue 3 jobs
    let job1 = queue
        .enqueue(&queue_name, "job", serde_json::json!({"n": 1}))
        .await?;
    let _job2 = queue
        .enqueue(&queue_name, "job", serde_json::json!({"n": 2}))
        .await?;
    let _job3 = queue
        .enqueue(&queue_name, "job", serde_json::json!({"n": 3}))
        .await?;

    // Worker 1 claims a job
    let claimed1 = queue.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(claimed1.id, job1);

    // Worker 2 should be able to claim a different job (not blocked by worker 1's lock)
    let claimed2 = queue.claim(&queue_name, "worker-2").await?.unwrap();
    assert_ne!(claimed2.id, job1);
    assert_eq!(claimed2.worker_id, Some("worker-2".to_string()));

    // Worker 1 cannot complete worker 2's job
    let result = queue.complete(claimed2.id, "worker-1").await;
    assert!(result.is_err());

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_priority_ordering() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("priority");

    use tokio_pgqueue::EnqueueOptions;

    // Enqueue low priority job first
    let low_priority = queue
        .enqueue_with_options(
            &queue_name,
            "job",
            serde_json::json!({"priority": "low"}),
            EnqueueOptions::new().priority(200),
        )
        .await?;

    // Then high priority
    let high_priority = queue
        .enqueue_with_options(
            &queue_name,
            "job",
            serde_json::json!({"priority": "high"}),
            EnqueueOptions::new().priority(1),
        )
        .await?;

    // Should claim high priority first
    let claimed = queue.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(
        claimed.id, high_priority,
        "High priority job should be claimed first"
    );

    let claimed = queue.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(
        claimed.id, low_priority,
        "Low priority job should be claimed second"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_dlq_operations() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("dlq");

    use tokio_pgqueue::EnqueueOptions;

    // Enqueue a job with max 1 attempt
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "job",
            serde_json::json!({"dlq": true}),
            EnqueueOptions::new().max_attempts(1),
        )
        .await?;

    // Claim and fail the job immediately -> should go to DLQ
    queue.claim(&queue_name, "worker-1").await?;
    queue.fail(job_id, "worker-1", "Immediate failure").await?;

    // Job should be in DLQ
    let count = queue.dlq_count(&queue_name).await?;
    assert_eq!(count, 1);

    // Drain the DLQ
    let dead_jobs = queue.drain_dlq(&queue_name, 10).await?;
    assert_eq!(dead_jobs.len(), 1);
    assert_eq!(dead_jobs[0].original_job_id, job_id);
    assert_eq!(
        dead_jobs[0].error_message.as_deref(),
        Some("Immediate failure")
    );

    // Requeue the dead job
    let new_job_id = queue.requeue_dlq(dead_jobs[0].id).await?;
    assert_ne!(new_job_id, job_id); // Should get a new ID

    // DLQ should be empty now
    let count = queue.dlq_count(&queue_name).await?;
    assert_eq!(count, 0);

    // New job should be claimable
    let job = queue.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(job.id, new_job_id);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_exponential_backoff() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    // Use default exponential backoff
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("backoff");

    use tokio_pgqueue::EnqueueOptions;

    // Enqueue a job with 5 max attempts
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "job",
            serde_json::json!({"backoff": true}),
            EnqueueOptions::new().max_attempts(5),
        )
        .await?;

    // First attempt - fail it
    queue.claim(&queue_name, "worker-1").await?;
    queue.fail(job_id, "worker-1", "Attempt 1 failed").await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.attempts, 1, "Should have 1 attempt after first failure");
    assert_eq!(job.status, JobStatus::Pending);
    
    // scheduled_at should be in the future due to backoff: 2 * 2^1 = 4 seconds
    // With base=2, attempt=1: delay = 2 * 2^1 = 4 seconds
    let now = chrono::Utc::now();
    let min_expected = now + chrono::Duration::seconds(3); // at least 3 seconds
    assert!(
        job.scheduled_at > min_expected,
        "Job should be scheduled for future (backoff): scheduled_at={}, now={}",
        job.scheduled_at, now
    );

    // Claim should not return this job immediately (it's in backoff)
    let immediate_claim = queue.claim(&queue_name, "worker-1").await?;
    assert!(
        immediate_claim.is_none(),
        "Job in backoff should not be claimable immediately"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_default_max_attempts_is_five() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("default_max");

    // Enqueue without specifying max_attempts
    let job_id = queue
        .enqueue(&queue_name, "job", serde_json::json!({}))
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(
        job.max_attempts, 5,
        "Default max_attempts should be 5"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_dlq_after_max_attempts() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    // Use no backoff for faster testing
    let config = QueueConfig {
        backoff_strategy: BackoffStrategy::None,
        ..QueueConfig::default()
    };
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("dlq_max");

    use tokio_pgqueue::EnqueueOptions;

    // Enqueue with exactly 5 max attempts
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "job",
            serde_json::json!({"test": "dlq_after_5"}),
            EnqueueOptions::new().max_attempts(5),
        )
        .await?;

    // Fail the job 5 times
    for i in 1..=5 {
        queue.claim(&queue_name, "worker-1").await?;
        queue.fail(job_id, "worker-1", &format!("Attempt {}", i)).await?;
        
        if i < 5 {
            let job = queue.get_job(job_id).await?.unwrap();
            assert_eq!(job.attempts, i, "After fail {}, attempts should be {}", i, i);
            assert_eq!(job.status, JobStatus::Pending);
        }
    }

    // After 5th failure, job should be in DLQ, not main queue
    let job = queue.get_job(job_id).await?;
    assert!(
        job.is_none(),
        "Job should be moved to DLQ after max_attempts (5)"
    );

    // Verify it's in DLQ
    let dlq_jobs = queue.drain_dlq(&queue_name, 10).await?;
    assert_eq!(dlq_jobs.len(), 1, "DLQ should have exactly 1 job");
    assert_eq!(dlq_jobs[0].original_job_id, job_id);
    assert_eq!(dlq_jobs[0].attempts, 5);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_backoff_increases_each_attempt() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("backoff_increase");

    use tokio_pgqueue::EnqueueOptions;

    // Enqueue with multiple attempts
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "job",
            serde_json::json!({}),
            EnqueueOptions::new().max_attempts(5),
        )
        .await?;

    let mut prev_delay_secs = 0i64;

    // Fail multiple times and verify backoff increases
    for attempt in 1..=3 {
        queue.claim(&queue_name, "worker-1").await?;
        let before_fail = chrono::Utc::now();
        queue.fail(job_id, "worker-1", &format!("Attempt {}", attempt)).await?;
        
        let job = queue.get_job(job_id).await?.unwrap();
        let delay_secs = (job.scheduled_at - before_fail).num_seconds();
        
        assert!(
            delay_secs > prev_delay_secs,
            "Backoff should increase: attempt {} delay {}s, previous {}s",
            attempt, delay_secs, prev_delay_secs
        );
        
        prev_delay_secs = delay_secs;
        
        // Manually reset scheduled_at for next iteration (simulating time passing)
        sqlx::query(
            "UPDATE job_queue SET scheduled_at = NOW() - INTERVAL '1 second' WHERE id = $1",
        )
        .bind(job_id)
        .execute(queue.pool())
        .await?;
    }

    Ok(())
}
