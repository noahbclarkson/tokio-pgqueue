-- Migration: Retry with exponential backoff enhancements
-- 
-- This migration updates defaults for improved retry behavior:
-- - Changes default max_attempts from 3 to 5
-- - The attempts column tracks retry attempt_count (0-indexed)
-- - Backoff is calculated as: base_delay * 2^attempt
-- - Jobs move to DLQ after max_attempts is exceeded

-- Update default max_attempts to 5 for better retry resilience
ALTER TABLE job_queue ALTER COLUMN max_attempts SET DEFAULT 5;

-- Add index for efficient backoff-aware claiming (jobs ready to be processed)
-- This supports the claim() query which filters on scheduled_at <= NOW()
CREATE INDEX IF NOT EXISTS idx_job_queue_claimable 
ON job_queue(queue_name, status, priority, scheduled_at) 
WHERE status = 'pending';
