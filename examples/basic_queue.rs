use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;
use sqlx::postgres::PgPoolOptions;
use tracing::{info, error};
use tokio_pgqueue::{PgQueue, EnqueueOptions, Worker, Job};

#[derive(Serialize, Deserialize, Debug)]
struct EmailJob {
    to: String,
    subject: String,
    body: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing to see the inner logs of tokio-pgqueue
    tracing_subscriber::fmt::init();

    // Get the database URL from the environment or use a default
    let db_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/tokio_pgqueue_example".to_string());

    info!("Connecting to database: {}", db_url);
    
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    info!("Database connected successfully.");

    // Create a new queue instance pointing to the 'email_queue'
    let queue = PgQueue::new(pool, "email_queue");

    // Initialize the queue schema (creates the table if it doesn't exist)
    queue.init().await?;
    info!("Queue initialized.");

    // 1. Enqueue a job
    let payload = EmailJob {
        to: "user@example.com".to_string(),
        subject: "Welcome!".to_string(),
        body: "Thanks for signing up.".to_string(),
    };

    let job_id = queue.enqueue(
        serde_json::to_value(&payload)?,
        EnqueueOptions::default()
            .with_max_retries(3) // Allow up to 3 retries if the job fails
    ).await?;

    info!("Enqueued job with ID: {}", job_id);

    // 2. Set up a worker to process jobs
    let worker_queue = queue.clone();
    let mut worker = Worker::new(worker_queue);
    
    info!("Starting worker... (Waiting for jobs)");
    
    // We'll run the worker in a separate task
    let worker_handle = tokio::spawn(async move {
        // process_jobs will run forever, polling for new jobs
        worker.process_jobs(|job: Job| async move {
            info!("Worker claimed job: {}", job.id);
            
            // Try to deserialize the payload
            match serde_json::from_value::<EmailJob>(job.payload.clone()) {
                Ok(email) => {
                    info!("Sending email to: {}", email.to);
                    info!("Subject: {}", email.subject);
                    
                    // Simulate some work (e.g., sending an actual email)
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    
                    info!("Email sent successfully!");
                    Ok(()) // Return Ok to mark the job as completed
                }
                Err(e) => {
                    error!("Failed to parse job payload: {}", e);
                    // Return Err to mark the job as failed. 
                    // It will be retried if retry_count < max_retries.
                    // If it exceeds max_retries, it will be marked as 'dead' (DLQ).
                    Err(anyhow::anyhow!("Payload parse error: {}", e))
                }
            }
        }).await;
    });

    // Let the worker run for a few seconds to process the job
    tokio::time::sleep(Duration::from_secs(3)).await;

    // To demonstrate the Dead Letter Queue (DLQ) behavior, let's enqueue a failing job
    info!("Enqueueing a job that will fail...");
    let bad_payload = serde_json::json!({
        "bad_format": true
    });
    
    let bad_job_id = queue.enqueue(
        bad_payload,
        EnqueueOptions::default()
            .with_max_retries(1) // Only retry once
    ).await?;
    
    info!("Enqueued bad job with ID: {}", bad_job_id);

    // Wait enough time for the bad job to fail and be retried
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Shutdown the worker gracefully (in a real app you'd wait for a shutdown signal)
    info!("Shutting down worker...");
    worker_handle.abort();
    
    info!("Example completed successfully.");
    Ok(())
}
