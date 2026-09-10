-- Persistent timing and resource history for each processing attempt.
-- Samples are deliberately small and remain with the shoot until its index is
-- deleted, matching the lifetime of the analysis records they describe.
CREATE TABLE processing_runs (
    id                INTEGER PRIMARY KEY,
    shoot_id          INTEGER NOT NULL REFERENCES shoots(id) ON DELETE CASCADE,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running', 'completed', 'failed', 'cancelled')),
    started_at        TEXT NOT NULL,
    scan_completed_at TEXT,
    completed_at      TEXT
);

-- At most one unfinished attempt per shoot. This also makes simultaneous
-- workers racing to start the first file safe.
CREATE UNIQUE INDEX idx_processing_runs_active
    ON processing_runs(shoot_id) WHERE status = 'running';
CREATE INDEX idx_processing_runs_shoot_started
    ON processing_runs(shoot_id, started_at DESC);

CREATE TABLE processing_stage_runs (
    id                INTEGER PRIMARY KEY,
    processing_run_id INTEGER NOT NULL REFERENCES processing_runs(id) ON DELETE CASCADE,
    stage             TEXT NOT NULL,
    started_at        TEXT NOT NULL,
    completed_at      TEXT,
    UNIQUE(processing_run_id, stage)
);

CREATE TABLE processing_resource_samples (
    id                INTEGER PRIMARY KEY,
    processing_run_id INTEGER NOT NULL REFERENCES processing_runs(id) ON DELETE CASCADE,
    recorded_at       TEXT NOT NULL,
    elapsed_ms        INTEGER NOT NULL,
    cpu_percent       REAL,
    gpu_percent       REAL,
    active_workers    INTEGER NOT NULL DEFAULT 0,
    concurrent_shoots INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX idx_processing_samples_run_time
    ON processing_resource_samples(processing_run_id, elapsed_ms);
