-- Leases on the job queue.
--
-- `requeue_stale` used to return *every* `running` row to `queued` at startup,
-- which is correct while one process owns the queue ("anything running must be
-- mine, and I just died") and a data-loss bug the moment two processes share
-- it: a laptop starting up would requeue jobs another machine is actively
-- running, and both would then run them.
--
-- Each claim now records who holds the job, a fencing token issued at claim
-- time, and when the lease expires. Workers heartbeat to extend it; a reaper
-- returns expired leases to the queue; and every write a worker makes is gated
-- on the token, so a worker whose lease has already been reaped cannot write.

ALTER TABLE jobs
  ADD COLUMN owner            TEXT,                        -- machine id holding the lease
  ADD COLUMN lease_token      TEXT,                        -- fencing token, issued at claim
  ADD COLUMN lease_expires_at TEXT,                        -- RFC3339 UTC, like every *_at column
  ADD COLUMN lease_losses     BIGINT NOT NULL DEFAULT 0;   -- how often the lease expired under a worker

CREATE INDEX idx_jobs_lease ON jobs (lease_expires_at) WHERE state = 'running';

-- Anything running at the moment this migration applies was claimed by the
-- single process that existed before leases, and that process is not running
-- this migration mid-job. Return it to the queue once, here, so no row is ever
-- `running` with a NULL lease — the reaper keys off `lease_expires_at` and
-- would otherwise never see it.
UPDATE jobs SET state = 'queued', started_at = NULL WHERE state = 'running';
