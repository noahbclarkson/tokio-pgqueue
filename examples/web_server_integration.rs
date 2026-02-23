use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::sync::Arc;
use tokio_pgqueue::{Job, PgQueue, QueueConfig, WorkerBuilder};
use tracing::{error, info};

// Simulate the Axum state extraction pattern
// Real axum code would use #[derive(Clone)] and the axum::extract::State struct
#[derive(Clone)]
struct AppState {
    queue: Arc<PgQueue>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ExportRequest {
    user_id: String,
    format: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing to see logs
    tracing_subscriber::fmt::init();

    // 1. Establish Database Connection (Startup Sequence step 1: Pool)
    let db_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/tokio_pgqueue_example".to_string());

    info!("Connecting to database: {}", db_url);
    
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    // 2. Initialize Queue (Startup Sequence step 2: Queue)
    let config = QueueConfig::default();
    let queue = PgQueue::new(pool, config).await?;
    let queue_name = "export_queue";

    info!("Queue initialized.");

    // Create shared state, which could be injected into axum route handlers
    let state = AppState {
        queue: Arc::new(queue.clone()),
    };

    // 3. Start Background Worker (Startup Sequence step 3: Start worker)
    let worker_queue = queue.clone();
    let worker = WorkerBuilder::new(queue_name, "worker-background-1")
        .queue(worker_queue)
        .handler_fn(|job: Job| async move {
            info!("Background worker claimed job: {}", job.id);
            
            // Try parsing payload
            let req: ExportRequest = serde_json::from_value(job.payload)?;
            info!("Processing export for user {} in format {}", req.user_id, req.format);
            
            // Simulate work (e.g., generating CSV/PDF)
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            
            info!("Export generation completed for user {}", req.user_id);
            Ok(())
        })
        .build()?;

    let worker_handle = tokio::spawn(async move {
        info!("Starting background worker task...");
        if let Err(e) = worker.run().await {
            error!("Worker error: {:?}", e);
        }
    });

    // 4. Start Web Server (Startup Sequence step 4: Start web server)
    // We simulate axum handling HTTP requests by spawning tasks.
    info!("Starting simulated web server...");
    
    // Simulate incoming HTTP requests that enqueue jobs via the shared state
    for i in 1..=3 {
        let state_clone = state.clone();
        tokio::spawn(async move {
            // Wait slightly before 'receiving' the request
            tokio::time::sleep(std::time::Duration::from_millis(500 * i)).await;
            
            // This represents axum's handler function
            let request_payload = ExportRequest {
                user_id: format!("user-{i}"),
                format: "csv".to_string(),
            };
            
            info!("[Web Handler] Received request to export data for user-{}", i);
            
            // Enqueue job via shared state
            match state_clone.queue.enqueue(
                "export_queue", 
                "generate_export", 
                serde_json::to_value(&request_payload).unwrap()
            ).await {
                Ok(job_id) => {
                    info!("[Web Handler] Successfully enqueued background job: {}", job_id);
                    // Real handler would return `Json({ "job_id": job_id, "status": "accepted" })`
                }
                Err(e) => {
                    error!("[Web Handler] Failed to enqueue background job: {:?}", e);
                    // Real handler would return a 500 internal server error
                }
            }
        });
    }

    // Give time for simulated requests and background processing to complete
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    // Gracefully shutdown the worker and web server
    info!("Shutting down web server and background workers...");
    worker_handle.abort();

    info!("Example completed successfully.");
    Ok(())
}