use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type JobId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub queue_name: String,
    pub job_type: String,
    pub payload: serde_json::Value,
    pub status: JobStatus,
    pub priority: u8,
    pub attempts: u32,
    pub max_attempts: u32,
    pub scheduled_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub worker_id: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "job_status", rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Pending => write!(f, "pending"),
            JobStatus::Running => write!(f, "running"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
        }
    }
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// How long a job can be without a heartbeat before being considered orphaned
    pub heartbeat_timeout: std::time::Duration,

    /// How long to wait between poll attempts (for worker loop)
    pub poll_interval: std::time::Duration,

    /// How frequently to send heartbeats for claimed jobs
    pub heartbeat_interval: std::time::Duration,

    /// Strategy for calculating retry backoff when a job fails
    pub backoff_strategy: BackoffStrategy,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout: std::time::Duration::from_secs(300), // 5 minutes
            poll_interval: std::time::Duration::from_millis(500),
            heartbeat_interval: std::time::Duration::from_secs(30),
            backoff_strategy: BackoffStrategy::default(),
        }
    }
}

/// Options for enqueuing a job with custom settings.
#[derive(Debug, Clone)]
pub struct EnqueueOptions {
    /// Job priority (0-255, lower = higher priority, default: 100)
    pub priority: u8,
    /// When the job should become available for claiming (default: immediately)
    pub run_at: Option<DateTime<Utc>>,
    /// Maximum number of retry attempts (default: 3)
    pub max_attempts: Option<u32>,
}

impl Default for EnqueueOptions {
    fn default() -> Self {
        Self {
            priority: 100,
            run_at: None,
            max_attempts: None,
        }
    }
}

impl EnqueueOptions {
    /// Create new enqueue options with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the job priority (0-255, lower = higher priority).
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Schedule the job to run at a specific time.
    pub fn run_at(mut self, run_at: DateTime<Utc>) -> Self {
        self.run_at = Some(run_at);
        self
    }

    /// Schedule the job to run after a delay.
    pub fn run_after(self, delay: std::time::Duration) -> Self {
        let run_at = Utc::now()
            + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::seconds(0));
        self.run_at(run_at)
    }

    /// Set the maximum number of retry attempts.
    pub fn max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }
}

/// Strategy for calculating retry backoff delay when a job fails.
#[derive(Debug, Clone)]
pub enum BackoffStrategy {
    /// No delay - retry immediately
    None,
    /// Fixed delay between retries
    Fixed(std::time::Duration),
    /// Exponential backoff: delay = base_secs * (multiplier ^ attempt)
    /// Capped at max_secs
    Exponential {
        /// Base delay in seconds (default: 2)
        base_secs: u64,
        /// Maximum delay cap in seconds (default: 1024 = ~17 minutes)
        max_secs: u64,
    },
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self::Exponential {
            base_secs: 2,
            max_secs: 1024,
        }
    }
}

impl BackoffStrategy {
    /// Calculate the backoff duration for a given attempt number.
    pub fn duration(&self, attempt: u32) -> std::time::Duration {
        match self {
            BackoffStrategy::None => std::time::Duration::ZERO,
            BackoffStrategy::Fixed(duration) => *duration,
            BackoffStrategy::Exponential { base_secs, max_secs } => {
                // Calculate 2^attempt * base, capped at max
                let multiplier = 2u64.saturating_pow(attempt);
                let delay_secs = (*base_secs)
                    .saturating_mul(multiplier)
                    .min(*max_secs);
                std::time::Duration::from_secs(delay_secs)
            }
        }
    }
}

/// Configuration for dead letter queue behavior.
#[derive(Debug, Clone)]
pub struct DlqConfig {
    /// Max retries before moving to DLQ (None = use job-level max_attempts)
    pub max_attempts: Option<u32>,
    /// Whether to keep failed job payload in DLQ table
    pub preserve_payload: bool,
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            max_attempts: None,
            preserve_payload: true,
        }
    }
}

/// A job that has been moved to the dead letter queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadJob {
    pub id: Uuid,
    pub original_job_id: Uuid,
    pub queue_name: String,
    pub job_type: String,
    pub payload: Option<serde_json::Value>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub error_message: Option<String>,
    pub failed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
