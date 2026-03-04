use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_pgqueue::{PgQueue, QueueConfig};
use tracing::{info, Level};

/// Represents the data required to generate a forecast report
#[derive(Debug, Serialize, Deserialize, Clone)]
struct GenerateForecastTask {
    /// The client ID in Xero
    client_id: String,
    /// The tenant ID for the Xero organisation
    tenant_id: String,
    /// The forecast period in months
    period_months: u8,
    /// Optional specific model override
    model_override: Option<String>,
}

/// The result of a forecast generation
#[derive(Debug, Serialize, Deserialize)]
struct ForecastResult {
    report_url: String,
    confidence_score: f32,
    generated_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("Starting xero-ai-forecaster pilot architecture example");

    // Connect to the database
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/tokio_pgqueue_dev".to_string());
        
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    // Create a queue specifically for report generation jobs
    // In a real app, this might run on dedicated worker nodes
    let queue = PgQueue::new(
        pool.clone(),
        QueueConfig::default()
            // In a real app we'd configure retry via the QueueConfig or EnqueueOptions
    ).await?;

    // Spawn the worker task
    let worker_handle = tokio::spawn(async move {
        info!("Worker started, waiting for forecast tasks...");
        
        loop {
            // Dequeue tasks as they arrive
            match queue.claim("forecast_reports", "worker-1").await {
                Ok(Some(job)) => {
                    // Deserialize the payload
                    let task: GenerateForecastTask = match serde_json::from_value(job.payload.clone()) {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::error!("Failed to parse payload: {}", e);
                            let _ = queue.fail(job.id, "worker-1", "Invalid payload format").await;
                            continue;
                        }
                    };
                    
                    info!("Processing forecast generation for client: {}", task.client_id);
                    
                    // Simulate processing time (fetching Xero data, calling LLM API, generating PDF)
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    
                    // Periodically send a heartbeat
                    let _ = queue.heartbeat(job.id, "worker-1").await;
                    
                    // Simulate a successful generation
                    let result = ForecastResult {
                        report_url: format!("https://storage.example.com/reports/{}.pdf", job.id),
                        confidence_score: 0.92,
                        generated_at: chrono::Utc::now(),
                    };
                    
                    // Mark the job as complete and store the result
                    if let Err(e) = queue.complete(job.id, "worker-1").await {
                        tracing::error!("Failed to complete job {}: {}", job.id, e);
                    } else {
                        info!("Successfully generated report for client: {}", task.client_id);
                    }
                }
                Ok(None) => {
                    // Queue is empty, wait before polling again
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => {
                    tracing::error!("Error dequeuing job: {}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Enqueue some example tasks
    let test_queue = PgQueue::new(
        pool,
        QueueConfig::default(),
    ).await?;

    info!("Enqueuing test forecast tasks...");

    test_queue.enqueue(
        "forecast_reports",
        "generate_report",
        serde_json::to_value(GenerateForecastTask {
            client_id: "client_alpha_123".to_string(),
            tenant_id: "tenant_xero_456".to_string(),
            period_months: 12,
            model_override: None,
        })?,
    ).await?;

    test_queue.enqueue(
        "forecast_reports",
        "generate_report",
        serde_json::to_value(GenerateForecastTask {
            client_id: "client_beta_789".to_string(),
            tenant_id: "tenant_xero_012".to_string(),
            period_months: 24,
            model_override: Some("gpt-4-turbo".to_string()),
        })?,
    ).await?;

    // Wait a moment for jobs to be processed
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Shutdown worker (in reality, handled gracefully on process exit)
    worker_handle.abort();
    
    info!("Example completed successfully");

    Ok(())
}