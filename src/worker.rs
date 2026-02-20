use crate::{Job, PgQueue, Result};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::time::sleep;
use tracing::{error, info};

/// Type alias for a boxed async handler function.
pub type HandlerFn =
    Arc<dyn Fn(Job) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> + Send + Sync>;

/// A worker that continuously claims and processes jobs from a queue.
///
/// # Example
///
/// ```no_run
/// use tokio_pgqueue::{QueueWorker, WorkerBuilder};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let queue = unimplemented!();
/// let worker = WorkerBuilder::new("my_queue", "worker-1")
///     .queue(queue)
///     .handler_fn(|job| async move {
///         println!("Processing job: {:?}", job);
///         Ok(())
///     })
///     .build()?;
///
/// worker.run().await?;
/// # Ok(())
/// # }
/// ```
pub struct QueueWorker {
    queue: PgQueue,
    queue_name: String,
    worker_id: String,
    handler: HandlerFn,
    config: WorkerConfig,
}

/// Configuration for a queue worker.
#[derive(Clone)]
pub struct WorkerConfig {
    /// How often to poll for new jobs
    pub poll_interval: Duration,
    /// How often to send heartbeats for claimed jobs
    pub heartbeat_interval: Duration,
    /// Whether to automatically reclaim orphans on startup
    pub auto_reclaim: bool,
    /// Timeout for considering jobs as orphaned
    pub reclaim_timeout: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            heartbeat_interval: Duration::from_secs(30),
            auto_reclaim: true,
            reclaim_timeout: Duration::from_secs(300), // 5 minutes
        }
    }
}

/// Builder for creating a QueueWorker.
pub struct WorkerBuilder {
    queue_name: String,
    worker_id: String,
    config: WorkerConfig,
    handler: Option<HandlerFn>,
    queue: Option<PgQueue>,
}

impl WorkerBuilder {
    /// Create a new worker builder.
    ///
    /// # Arguments
    ///
    /// * `queue_name` - The name of the queue to process
    /// * `worker_id` - A unique identifier for this worker
    pub fn new(queue_name: impl Into<String>, worker_id: impl Into<String>) -> Self {
        Self {
            queue_name: queue_name.into(),
            worker_id: worker_id.into(),
            config: WorkerConfig::default(),
            handler: None,
            queue: None,
        }
    }

    /// Set the PgQueue instance to use.
    pub fn queue(mut self, queue: PgQueue) -> Self {
        self.queue = Some(queue);
        self
    }

    /// Set the poll interval.
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.config.poll_interval = interval;
        self
    }

    /// Set the heartbeat interval.
    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.config.heartbeat_interval = interval;
        self
    }

    /// Enable or disable automatic orphan reclamation.
    pub fn auto_reclaim(mut self, enabled: bool) -> Self {
        self.config.auto_reclaim = enabled;
        self
    }

    /// Set the timeout for orphan reclamation.
    pub fn reclaim_timeout(mut self, timeout: Duration) -> Self {
        self.config.reclaim_timeout = timeout;
        self
    }

    /// Set the job handler using an async closure or function.
    ///
    /// The handler receives a [`Job`] and must return a future resolving to `Result<()>`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tokio_pgqueue::WorkerBuilder;
    /// WorkerBuilder::new("my_queue", "worker-1")
    ///     .handler_fn(|job| async move {
    ///         println!("Processing: {:?}", job.job_type);
    ///         Ok(())
    ///     });
    /// ```
    pub fn handler_fn<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Job) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.handler = Some(Arc::new(move |job| {
            Box::pin(f(job)) as Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        }));
        self
    }

    /// Set the job handler using a pre-boxed function pointer.
    ///
    /// Use [`handler_fn`] for most cases. This variant accepts the raw [`HandlerFn`] arc
    /// for sharing a handler across multiple workers.
    pub fn handler(mut self, handler: HandlerFn) -> Self {
        self.handler = Some(handler);
        self
    }

    /// Build the worker.
    ///
    /// # Errors
    ///
    /// Returns an error if no handler or queue was set.
    pub fn build(self) -> Result<QueueWorker> {
        let handler = self
            .handler
            .ok_or_else(|| crate::QueueError::InvalidConfig("No handler set".to_string()))?;

        let queue = self.queue.ok_or_else(|| {
            crate::QueueError::InvalidConfig("No PgQueue instance provided".to_string())
        })?;

        Ok(QueueWorker {
            queue,
            queue_name: self.queue_name,
            worker_id: self.worker_id,
            handler,
            config: self.config,
        })
    }
}

impl QueueWorker {
    /// Run the worker indefinitely, processing jobs as they become available.
    ///
    /// This method will continue until the worker is cancelled or an unrecoverable error occurs.
    pub async fn run(&self) -> Result<()> {
        info!(
            "Worker {} started for queue {}",
            self.worker_id, self.queue_name
        );

        // Auto-reclaim orphans on startup if enabled
        if self.config.auto_reclaim {
            self.reclaim_orphans().await?;
        }

        loop {
            // Try to claim a job
            match self.queue.claim(&self.queue_name, &self.worker_id).await? {
                Some(job) => {
                    // Spawn heartbeat task
                    let queue = self.queue.clone();
                    let job_id = job.id;
                    let worker_id = self.worker_id.clone();
                    let heartbeat_interval = self.config.heartbeat_interval;

                    let heartbeat_handle = tokio::spawn(async move {
                        loop {
                            sleep(heartbeat_interval).await;
                            if let Err(e) = queue.heartbeat(job_id, &worker_id).await {
                                error!("Heartbeat failed for job {}: {:?}", job_id, e);
                                break;
                            }
                        }
                    });

                    // Process the job
                    let result = (self.handler)(job.clone()).await;

                    // Abort heartbeat task
                    heartbeat_handle.abort();

                    // Report job completion/failure
                    match result {
                        Ok(()) => {
                            if let Err(e) = self.queue.complete(job.id, &self.worker_id).await {
                                error!("Failed to mark job {} as complete: {:?}", job.id, e);
                            }
                        }
                        Err(e) => {
                            error!("Job {} failed: {:?}", job.id, e);
                            if let Err(err) = self
                                .queue
                                .fail(job.id, &self.worker_id, &format!("{e:?}"))
                                .await
                            {
                                error!("Failed to mark job {} as failed: {:?}", job.id, err);
                            }
                        }
                    }
                }
                None => {
                    // No jobs available, wait before next poll
                    sleep(self.config.poll_interval).await;
                }
            }
        }
    }

    /// Reclaim orphaned jobs for this worker's queue.
    pub async fn reclaim_orphans(&self) -> Result<()> {
        let reclaimed = self
            .queue
            .reclaim_orphans(&self.queue_name, self.config.reclaim_timeout)
            .await?;

        if !reclaimed.is_empty() {
            info!(
                "Reclaimed {} orphaned jobs from queue {}",
                reclaimed.len(),
                self.queue_name
            );
        }

        Ok(())
    }
}
