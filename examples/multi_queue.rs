use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::time::Duration;
use tokio_pgqueue::{Job, PgQueue, QueueConfig, WorkerBuilder};
use tracing::{error, info};

// Three different payload types for three distinct queues
#[derive(Serialize, Deserialize, Debug)]
struct EmailJob {
    to: String,
    body: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct NotificationJob {
    user_id: String,
    message: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct HeavyProcessingJob {
    video_id: String,
    resolution: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Setup database connection
    let db_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/tokio_pgqueue_example".to_string());

    info!("Connecting to database: {}", db_url);
    
    // 1. Create a shared `PgPool`
    let pool = PgPoolOptions::new()
        .max_connections(10) // Larger pool size to support multiple concurrent queues
        .connect(&db_url)
        .await?;

    info!("Database connected successfully.");

    // 2. Create 3 `PgQueue` instances pointing to the same pool but different queue names
    let config = QueueConfig::default();
    
    // We can wrap the queue in an Arc if we want to share it among multiple components,
    // though cloning `PgQueue` itself is also cheap (it just clones the underlying Pool).
    let email_queue = PgQueue::new(pool.clone(), config.clone()).await?;
    let notification_queue = PgQueue::new(pool.clone(), config.clone()).await?;
    let heavy_jobs_queue = PgQueue::new(pool, config).await?;

    info!("Queues initialized.");

    // 3. Enqueue different payload types to each queue
    let email = EmailJob {
        to: "user@example.com".to_string(),
        body: "Your weekly summary is here!".to_string(),
    };
    email_queue.enqueue("email_queue", "send_email", serde_json::to_value(&email)?).await?;

    let notification = NotificationJob {
        user_id: "user-123".to_string(),
        message: "You have 3 new messages".to_string(),
    };
    notification_queue.enqueue("notification_queue", "send_push", serde_json::to_value(&notification)?).await?;

    let processing = HeavyProcessingJob {
        video_id: "vid-456".to_string(),
        resolution: "1080p".to_string(),
    };
    heavy_jobs_queue.enqueue("heavy_jobs", "encode_video", serde_json::to_value(&processing)?).await?;

    info!("All jobs enqueued. Starting workers...");

    // 4. Spawn 3 separate worker tasks via `tokio::spawn`, each handling its own queue
    
    let worker_email = WorkerBuilder::new("email_queue", "worker-email-1")
        .queue(email_queue.clone())
        .handler_fn(|job: Job| async move {
            let email: EmailJob = serde_json::from_value(job.payload)?;
            info!("[Email Worker] Sending email to {}", email.to);
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        })
        .build()?;

    let worker_notification = WorkerBuilder::new("notification_queue", "worker-notification-1")
        .queue(notification_queue.clone())
        .handler_fn(|job: Job| async move {
            let notif: NotificationJob = serde_json::from_value(job.payload)?;
            info!("[Notification Worker] Sending push to user {}", notif.user_id);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(())
        })
        .build()?;

    let worker_heavy = WorkerBuilder::new("heavy_jobs", "worker-heavy-1")
        .queue(heavy_jobs_queue.clone())
        .handler_fn(|job: Job| async move {
            let proc: HeavyProcessingJob = serde_json::from_value(job.payload)?;
            info!("[Heavy Worker] Encoding video {} at {}", proc.video_id, proc.resolution);
            // Simulate long-running job
            tokio::time::sleep(Duration::from_secs(2)).await;
            info!("[Heavy Worker] Done encoding video");
            Ok(())
        })
        .build()?;

    let email_task = tokio::spawn(async move {
        if let Err(e) = worker_email.run().await {
            error!("Email worker error: {:?}", e);
        }
    });

    let notif_task = tokio::spawn(async move {
        if let Err(e) = worker_notification.run().await {
            error!("Notification worker error: {:?}", e);
        }
    });

    let heavy_task = tokio::spawn(async move {
        if let Err(e) = worker_heavy.run().await {
            error!("Heavy jobs worker error: {:?}", e);
        }
    });

    // 5 & 6. Show graceful shutdown with `tokio::signal::ctrl_c()`
    // Here we use `tokio::select!` to race the ctrl-c signal against letting them run.
    info!("Workers running. Press Ctrl+C to shut down.");

    // For the sake of the example, we'll auto-terminate after a few seconds
    // to allow the example to finish without manual intervention if run via `cargo run --example`.
    // In a real app, you would only wait on `tokio::signal::ctrl_c()`.
    
    let auto_shutdown_timeout = tokio::time::sleep(Duration::from_secs(5));
    
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal. Gracefully shutting down workers...");
        }
        _ = auto_shutdown_timeout => {
            info!("Example complete (auto-shutdown). Shutting down workers...");
        }
    }

    // Abort all worker tasks
    email_task.abort();
    notif_task.abort();
    heavy_task.abort();

    info!("Workers shut down. Exiting.");
    Ok(())
}