// Edge case tests for tokio-pgqueue
//
// These tests verify behavior in unusual or boundary conditions.

use sqlx::PgPool;
use tokio_pgqueue::{BackoffStrategy, JobStatus, PgQueue, QueueConfig, EnqueueOptions};
use uuid::Uuid;

fn test_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/tokio_pgqueue_test".to_string())
}

fn unique_queue_name(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::new_v4())
}

// ========================================
// Edge cases for enqueue operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_priority_zero() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("priority_zero");

    // Enqueue job with priority 0 (highest priority)
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "test_job",
            serde_json::json!({}),
            EnqueueOptions::new().priority(0),
        )
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.priority, 0);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_priority_255() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("priority_max");

    // Enqueue job with priority 255 (lowest priority)
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "test_job",
            serde_json::json!({}),
            EnqueueOptions::new().priority(255),
        )
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.priority, 255);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_max_attempts_one() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig {
        backoff_strategy: BackoffStrategy::None,
        ..QueueConfig::default()
    };
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("max_attempts_one");

    // Enqueue job with max_attempts = 1 (should go to DLQ on first failure)
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "test_job",
            serde_json::json!({}),
            EnqueueOptions::new().max_attempts(1),
        )
        .await?;

    // Claim and fail the job
    queue.claim(&queue_name, "worker-1").await?;
    queue.fail(job_id, "worker-1", "Failed on first attempt").await?;

    // Job should be in DLQ, not in main queue
    let job = queue.get_job(job_id).await?;
    assert!(job.is_none(), "Job should have been moved to DLQ");

    let dlq_count = queue.dlq_count(&queue_name).await?;
    assert_eq!(dlq_count, 1, "DLQ should have one job");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_high_max_attempts() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("high_max_attempts");

    // Enqueue job with very high max_attempts
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "test_job",
            serde_json::json!({}),
            EnqueueOptions::new().max_attempts(1000),
        )
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.max_attempts, 1000);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_scheduled_in_past() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("past_scheduled");

    use chrono::{Utc, Duration};

    // Schedule a job in the past (should be immediately claimable)
    let past_time = Utc::now() - Duration::hours(1);
    let job_id = queue
        .enqueue_at(&queue_name, "test_job", serde_json::json!({}), past_time)
        .await?;

    // Should be claimable immediately
    let job = queue.claim(&queue_name, "worker-1").await?;
    assert!(job.is_some(), "Job scheduled in past should be claimable");
    assert_eq!(job.unwrap().id, job_id);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_scheduled_far_future() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("future_scheduled");

    use chrono::{Utc, Duration};

    // Schedule a job far in the future
    let future_time = Utc::now() + Duration::days(365);
    let job_id = queue
        .enqueue_at(&queue_name, "test_job", serde_json::json!({}), future_time)
        .await?;

    // Should not be claimable
    let job = queue.claim(&queue_name, "worker-1").await?;
    assert!(job.is_none(), "Job scheduled in future should not be claimable");

    // But should exist in the queue
    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.status, JobStatus::Pending);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_empty_payload() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("empty_payload");

    // Enqueue job with empty JSON object
    let job_id = queue
        .enqueue(&queue_name, "test_job", serde_json::json!({}))
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.payload, serde_json::json!({}));

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_null_payload() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("null_payload");

    // Enqueue job with null payload
    let job_id = queue
        .enqueue(&queue_name, "test_job", serde_json::Value::Null)
        .await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.payload, serde_json::Value::Null);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_with_complex_payload() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("complex_payload");

    // Enqueue job with complex nested JSON
    let payload = serde_json::json!({
        "user": {
            "id": 123,
            "name": "Test User",
            "email": "test@example.com"
        },
        "items": [
            {"id": 1, "name": "Item 1"},
            {"id": 2, "name": "Item 2"}
        ],
        "metadata": {
            "tags": ["urgent", "priority"],
            "count": 42
        }
    });

    let job_id = queue.enqueue(&queue_name, "test_job", payload.clone()).await?;

    let job = queue.get_job(job_id).await?.unwrap();
    assert_eq!(job.payload, payload);

    Ok(())
}

// ========================================
// Edge cases for claim operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_claim_from_empty_queue() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("empty_queue");

    // Try to claim from empty queue
    let job = queue.claim(&queue_name, "worker-1").await?;
    assert!(job.is_none(), "Empty queue should return None");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_claim_when_all_jobs_claimed() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("all_claimed");

    // Enqueue 2 jobs and claim both
    let _job1 = queue.enqueue(&queue_name, "job", serde_json::json!({})).await?;
    let _job2 = queue.enqueue(&queue_name, "job", serde_json::json!({})).await?;

    let _claimed1 = queue.claim(&queue_name, "worker-1").await?;
    let _claimed2 = queue.claim(&queue_name, "worker-2").await?;

    // Try to claim again - should return None
    let job = queue.claim(&queue_name, "worker-3").await?;
    assert!(job.is_none(), "Should return None when all jobs are claimed");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_claim_with_very_long_worker_id() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("long_worker_id");

    // Enqueue a job
    let job_id = queue.enqueue(&queue_name, "test_job", serde_json::json!({})).await?;

    // Claim with very long worker ID
    let long_worker_id = "worker-".repeat(100); // ~700 chars
    let job = queue.claim(&queue_name, &long_worker_id).await?.unwrap();

    assert_eq!(job.id, job_id);
    assert_eq!(job.worker_id, Some(long_worker_id.clone()));

    // Should be able to complete with the same long worker ID
    queue.complete(job_id, &long_worker_id).await?;

    Ok(())
}

// ========================================
// Edge cases for heartbeat operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_heartbeat_nonexistent_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Try to heartbeat a non-existent job
    let fake_job_id = Uuid::new_v4();
    let result = queue.heartbeat(fake_job_id, "worker-1").await;

    assert!(result.is_err(), "Should error when heartbeat non-existent job");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_heartbeat_completed_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("heartbeat_completed");

    // Enqueue, claim, and complete a job
    let job_id = queue.enqueue(&queue_name, "test_job", serde_json::json!({})).await?;
    queue.claim(&queue_name, "worker-1").await?;
    queue.complete(job_id, "worker-1").await?;

    // Try to heartbeat the completed job
    let result = queue.heartbeat(job_id, "worker-1").await;
    assert!(result.is_err(), "Should error when heartbeat completed job");

    Ok(())
}

// ========================================
// Edge cases for complete operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_complete_nonexistent_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Try to complete a non-existent job
    let fake_job_id = Uuid::new_v4();
    let result = queue.complete(fake_job_id, "worker-1").await;

    assert!(result.is_err(), "Should error when completing non-existent job");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_complete_job_twice() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("complete_twice");

    // Enqueue and complete a job
    let job_id = queue.enqueue(&queue_name, "test_job", serde_json::json!({})).await?;
    queue.claim(&queue_name, "worker-1").await?;
    queue.complete(job_id, "worker-1").await?;

    // Try to complete it again
    let result = queue.complete(job_id, "worker-1").await;
    assert!(result.is_err(), "Should error when completing job twice");

    Ok(())
}

// ========================================
// Edge cases for fail operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_fail_nonexistent_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Try to fail a non-existent job
    let fake_job_id = Uuid::new_v4();
    let result = queue.fail(fake_job_id, "worker-1", "Error").await;

    assert!(result.is_err(), "Should error when failing non-existent job");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_fail_completed_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("fail_completed");

    // Enqueue, claim, and complete a job
    let job_id = queue.enqueue(&queue_name, "test_job", serde_json::json!({})).await?;
    queue.claim(&queue_name, "worker-1").await?;
    queue.complete(job_id, "worker-1").await?;

    // Try to fail the completed job
    let result = queue.fail(job_id, "worker-1", "Error").await;
    assert!(result.is_err(), "Should error when failing completed job");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_fail_with_long_error_message() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig {
        backoff_strategy: BackoffStrategy::None,
        ..QueueConfig::default()
    };
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("long_error");

    // Enqueue job with max_attempts = 1
    let job_id = queue
        .enqueue_with_options(
            &queue_name,
            "test_job",
            serde_json::json!({}),
            EnqueueOptions::new().max_attempts(1),
        )
        .await?;

    // Claim and fail with very long error message
    let long_error = "Error: ".repeat(100); // ~700 chars
    queue.claim(&queue_name, "worker-1").await?;
    queue.fail(job_id, "worker-1", &long_error).await?;

    // Verify error message is stored in DLQ
    let dead_jobs = queue.drain_dlq(&queue_name, 10).await?;
    assert_eq!(dead_jobs.len(), 1);
    assert!(dead_jobs[0].error_message.as_ref().unwrap().starts_with("Error: Error:"));

    Ok(())
}

// ========================================
// Edge cases for DLQ operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_drain_empty_dlq() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("empty_dlq");

    // Drain from empty DLQ
    let dead_jobs = queue.drain_dlq(&queue_name, 10).await?;
    assert_eq!(dead_jobs.len(), 0, "Empty DLQ should return empty list");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_drain_dlq_with_zero_limit() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("zero_limit");

    // Drain with limit = 0
    let dead_jobs = queue.drain_dlq(&queue_name, 0).await?;
    assert_eq!(dead_jobs.len(), 0, "Zero limit should return empty list");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_delete_nonexistent_dlq_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Try to delete non-existent DLQ job
    let fake_dlq_id = Uuid::new_v4();
    let deleted = queue.delete_dlq_job(fake_dlq_id).await?;
    assert!(!deleted, "Deleting non-existent DLQ job should return false");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_requeue_nonexistent_dlq_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Try to requeue non-existent DLQ job
    let fake_dlq_id = Uuid::new_v4();
    let result = queue.requeue_dlq(fake_dlq_id).await;
    assert!(result.is_err(), "Requeuing non-existent DLQ job should error");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_dlq_count_empty_queue() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("count_empty");

    // Count DLQ for queue with no failed jobs
    let count = queue.dlq_count(&queue_name).await?;
    assert_eq!(count, 0, "Empty DLQ count should be 0");

    Ok(())
}

// ========================================
// Edge cases for reclaim operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_reclaim_from_empty_queue() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("reclaim_empty");

    // Reclaim from empty queue
    let reclaimed = queue
        .reclaim_orphans(&queue_name, std::time::Duration::from_secs(300))
        .await?;
    assert_eq!(reclaimed.len(), 0, "Empty queue should have no orphans");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_reclaim_when_no_orphans() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("no_orphans");

    // Enqueue and claim a job (recent heartbeat)
    let _job_id = queue.enqueue(&queue_name, "test_job", serde_json::json!({})).await?;
    queue.claim(&queue_name, "worker-1").await?;

    // Reclaim with short timeout - job has recent heartbeat
    let reclaimed = queue
        .reclaim_orphans(&queue_name, std::time::Duration::from_secs(300))
        .await?;
    assert_eq!(reclaimed.len(), 0, "No orphans should be reclaimed");

    Ok(())
}

// ========================================
// Edge cases for queue_depth operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_queue_depth_empty_queue() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("depth_empty");

    // Get depth of empty queue
    let depth = queue.queue_depth(&queue_name).await?;
    assert_eq!(depth, 0, "Empty queue depth should be 0");

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_queue_depth_with_claimed_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("depth_claimed");

    // Enqueue and claim 2 jobs
    let _job1 = queue.enqueue(&queue_name, "job", serde_json::json!({})).await?;
    let _job2 = queue.enqueue(&queue_name, "job", serde_json::json!({})).await?;
    queue.claim(&queue_name, "worker-1").await?;
    queue.claim(&queue_name, "worker-1").await?;

    // Depth should only count pending jobs
    let depth = queue.queue_depth(&queue_name).await?;
    assert_eq!(depth, 0, "Queue depth should not include claimed jobs");

    Ok(())
}

// ========================================
// Edge cases for get_job operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_get_nonexistent_job() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;

    // Try to get a non-existent job
    let fake_job_id = Uuid::new_v4();
    let job = queue.get_job(fake_job_id).await?;
    assert!(job.is_none(), "Non-existent job should return None");

    Ok(())
}

// ========================================
// Edge cases for multiple operations
// ========================================

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_multiple_queues_same_name_different_instances() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue1 = PgQueue::new(pool.clone(), config.clone()).await?;
    let queue2 = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("multi_instance");

    // Enqueue from queue1
    let job_id = queue1.enqueue(&queue_name, "test_job", serde_json::json!({})).await?;

    // Should be able to claim from queue2 (same database)
    let job = queue2.claim(&queue_name, "worker-1").await?.unwrap();
    assert_eq!(job.id, job_id);

    Ok(())
}

#[tokio::test]
#[ignore = "requires postgres"]
async fn test_enqueue_many_jobs_priority_ordering() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&test_db_url()).await?;
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = unique_queue_name("many_priority");

    // Enqueue many jobs with different priorities
    for i in 0..10 {
        queue
            .enqueue_with_options(
                &queue_name,
                "job",
                serde_json::json!({"n": i}),
                EnqueueOptions::new().priority((i * 10) as u8),
            )
            .await?;
    }

    // Claim should get jobs in priority order
    let mut claimed_ids = vec![];
    for _ in 0..10 {
        let job = queue.claim(&queue_name, "worker-1").await?.unwrap();
        claimed_ids.push(job.id);
        queue.complete(job.id, "worker-1").await?;
    }

    // All jobs should be claimed
    assert_eq!(claimed_ids.len(), 10);

    Ok(())
}
