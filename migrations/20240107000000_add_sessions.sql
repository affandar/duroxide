-- Session table for worker affinity (activity-explicit-sessions).
-- Primary key is (instance_id, session_id) because sessions are instance-scoped.
CREATE TABLE IF NOT EXISTS sessions (
    instance_id   TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    worker_id     TEXT,              -- NULL = unclaimed, non-NULL = attached to a worker
    -- locked_until uses INTEGER (ms since epoch), same as instance_locks and worker_queue.
    locked_until  INTEGER,           -- NULL = unclaimed, else lock expiry (ms since epoch)
    PRIMARY KEY (instance_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_sessions_worker ON sessions(worker_id);
CREATE INDEX IF NOT EXISTS idx_sessions_locked_until ON sessions(locked_until);

-- Add session_id column to worker queue for session-bound routing.
ALTER TABLE worker_queue ADD COLUMN session_id TEXT;
