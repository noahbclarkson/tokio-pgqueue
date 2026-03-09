use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use std::env;
use tokio_pgqueue::{EnqueueOptions, Job, PgQueue, QueueConfig, WorkerBuilder};
use tracing::{error, info, warn};

/// A job payload for processing reports.
#[derive(Serialize, Deserialize, Debug)]
struct ReportJob {
    report_id: String,
    force_fail: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing for clear logging
    tracing_subscriber::fmt::init();

    // Setup database connection
    let db_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/tokio_pgqueue_example".to_string());

    info!("Connecting to database: {}", db_url);
    
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    let queue = PgQueue::new(pool, QueueConfig::default()).await?;
    let queue_name = "reports_queue";

    info!("Queue initialized.");

    // 1. Enqueue a normal job
    let normal_job = ReportJob {
        report_id: "report-normal-1".to_string(),
        force_fail: false,
    };
    let job_id = queue.enqueue(queue_name, "generate_report", serde_json::to_value(&normal_job)?).await?;
    info!("Enqueued normal job: {}", job_id);

    // 2. Enqueue a high-priority job
    // Lower priority number means higher priority in tokio-pgqueue
    let urgent_job = ReportJob {
        report_id: "report-urgent-1".to_string(),
        force_fail: false,
    };
    let urgent_options = EnqueueOptions::new().priority(10);
    let urgent_job_id = queue.enqueue_with_options(
        queue_name, 
        "generate_report", 
        serde_json::to_value(&urgent_job)?, 
        urgent_options
    ).await?;
    info!("Enqueued high-priority job: {}", urgent_job_id);

    // 3. Schedule a job 30 minutes in the future
    let scheduled_job = ReportJob {
        report_id: "report-scheduled-1".to_string(),
        force_fail: false,
    };
    let scheduled_options = EnqueueOptions::new().run_after(std::time::Duration::from_secs(1800));
    let scheduled_job_id = queue.enqueue_with_options(
        queue_name, 
        "generate_report", 
        serde_json::to_value(&scheduled_job)?, 
        scheduled_options
    ).await?;
    info!("Enqueued scheduled job (runs in 30 mins): {}", scheduled_job_id);

    // 4. Enqueue a job designed to fail (to demonstrate DLQ)
    let bad_job = ReportJob {
        report_id: "report-bad-1".to_string(),
        force_fail: true,
    };
    // Only attempt once so it goes straight to the DLQ when it fails
    let bad_options = EnqueueOptions::new().max_attempts(1);
    let bad_job_id = queue.enqueue_with_options(
        queue_name, 
        "generate_report", 
        serde_json::to_value(&bad_job)?, 
        bad_options
    ).await?;
    info!("Enqueued failing job: {}", bad_job_id);

    // 5. Run a worker that processes jobs
    let worker_queue = queue.clone();
    
    let worker_handle = tokio::spawn(async move {
        let worker = WorkerBuilder::new(queue_name, "worker-scheduled")
            .queue(worker_queue)
            .handler_fn(|job: Job| async move {
                info!("Worker claimed job: {} (priority: {})", job.id, job.priority);
                
                let report = serde_json::from_value::<ReportJob>(job.payload.clone())
                    .map_err(|e| tokio_pgqueue::QueueError::InvalidConfig(e.to_string()))?;
                
                if report.force_fail {
                    warn!("Simulating failure for report: {}", report.report_id);
                    return Err(tokio_pgqueue::QueueError::InvalidConfig("Simulated failure".to_string()));
                }

                info!("Processing report: {}", report.report_id);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                info!("Report {} completed successfully", report.report_id);
                
                Ok(())
            })
            .build()
            .expect("Failed to build worker");

        if let Err(e) = worker.run().await {
            error!("Worker error: {:?}", e);
        }
    });

    // Let the worker process the immediate jobs
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // 6. Check the DLQ after a failed job
    info!("Checking Dead Letter Queue (DLQ)...");
    let dlq_count = queue.dlq_count(queue_name).await?;
    info!("Found {} jobs in the DLQ.", dlq_count);

    if dlq_count > 0 {
        // Drain jobs from DLQ (limit 10)
        let dead_jobs = queue.drain_dlq(queue_name, 10).await?;
        
        for dead_job in dead_jobs {
            warn!("Found dead job: {} (original: {}). Error: {:?}", 
                dead_job.id, dead_job.original_job_id, dead_job.error_message);
            
            // Requeue from DLQ
            info!("Requeueing job {} from DLQ to give it another try...", dead_job.id);
            let new_job_id = queue.requeue_dlq(dead_job.id).await?;
            info!("Successfully requeued as new job: {}", new_job_id);
        }
    }

    // Let the worker process the requeued job (which will fail again since it's the same payload)
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Gracefully shutdown the worker
    info!("Shutting down worker...");
    worker_handle.abort();

    info!("Example completed successfully.");
    Ok(())
}
