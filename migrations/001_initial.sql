-- Create custom enum type for job status
CREATE TYPE job_status AS ENUM ('pending', 'running', 'completed', 'failed');

-- Create the job_queue table
CREATE TABLE job_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    queue_name TEXT NOT NULL,
    job_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    status job_status NOT NULL DEFAULT 'pending',
    attempts INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ,
    worker_id TEXT,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for efficiently finding pending jobs
CREATE INDEX idx_job_queue_pending ON job_queue(queue_name, status, scheduled_at)
    WHERE status = 'pending';

-- Index for efficiently finding running jobs (for heartbeat/timeout monitoring)
CREATE INDEX idx_job_queue_running ON job_queue(queue_name, status, last_heartbeat_at)
    WHERE status = 'running';

-- Index for worker_id lookup
CREATE INDEX idx_job_queue_worker ON job_queue(worker_id)
    WHERE worker_id IS NOT NULL;

-- Trigger to automatically update updated_at timestamp
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

CREATE TRIGGER update_job_queue_updated_at
    BEFORE UPDATE ON job_queue
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
