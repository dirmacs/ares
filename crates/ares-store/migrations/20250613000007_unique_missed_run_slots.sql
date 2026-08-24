-- Migration 20250613000007: Enforce one catch-up audit per missed slot
-- Prevents duplicate missed_runs rows when multiple scheduler ticks/processes
-- observe the same overdue schedule concurrently.

CREATE UNIQUE INDEX IF NOT EXISTS idx_missed_runs_schedule_expected
    ON missed_runs(schedule_id, expected_at);
