-- Runs written by migration 9 measured only the SKWAD parent process. New
-- runs measure total system CPU so FFmpeg child processes are included.
ALTER TABLE processing_runs
    ADD COLUMN cpu_metric_scope TEXT NOT NULL DEFAULT 'process';
