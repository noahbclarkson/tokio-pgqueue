-- Add priority column to job_queue
ALTER TABLE job_queue ADD COLUMN priority SMALLINT NOT NULL DEFAULT 100;

-- Create index for priority-based claiming
DROP INDEX idx_job_queue_pending;
CREATE INDEX idx_job_queue_pending ON job_queue(queue_name, status, priority, scheduled_at)
    WHERE status = 'pending';

-- Create dead letter queue table
CREATE TABLE dlq_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    original_job_id UUID NOT NULL,
    queue_name TEXT NOT NULL,
    job_type TEXT NOT NULL,
    payload JSONB,
    attempts INT NOT NULL,
    max_attempts INT NOT NULL,
    error_message TEXT,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for DLQ lookups
CREATE INDEX idx_dlq_jobs_queue ON dlq_jobs(queue_name, failed_at);
