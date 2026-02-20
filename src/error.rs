use thiserror::Error;

pub type Result<T> = std::result::Result<T, QueueError>;

#[derive(Error, Debug)]
pub enum QueueError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Job not found: {0}")]
    JobNotFound(uuid::Uuid),

    #[error("Job ownership verification failed for job {job_id} (claimed by {claimed_by}, worker is {worker_id})")]
    OwnershipViolation {
        job_id: uuid::Uuid,
        claimed_by: String,
        worker_id: String,
    },

    #[error("Job {0} has exceeded maximum retry attempts")]
    MaxAttemptsExceeded(uuid::Uuid),

    #[error("Job {0} is not in the expected state")]
    InvalidJobState(uuid::Uuid),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}
