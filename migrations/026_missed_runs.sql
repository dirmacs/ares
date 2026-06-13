-- Migration 026: Missed runs audit table
-- Supports DIR1-73: Configurable missed-run grace period

CREATE TABLE IF NOT EXISTS missed_runs (
    id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL,
    expected_at BIGINT NOT NULL,
    detected_at BIGINT NOT NULL,
    action_taken TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    FOREIGN KEY (schedule_id) REFERENCES agent_schedules(id) ON DELETE CASCADE
);

CREATE INDEX idx_missed_runs_schedule ON missed_runs(schedule_id);
CREATE INDEX idx_missed_runs_detected ON missed_runs(detected_at DESC);