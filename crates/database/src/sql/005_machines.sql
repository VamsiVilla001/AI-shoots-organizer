-- Machines enrolled as workers.
--
-- Worker traffic and UI traffic want different identities: if a worker
-- authenticated with the signed-in user's session, a machine that should be
-- contributing would go idle the moment that person logged out. So machines
-- are enrolled separately, by an administrator, and present their own token.
-- `jobs.owner` is already the machine id for the lease, so this table joined
-- to `jobs` *is* the worker roster. Revocation is one column, effective at
-- the next claim.

CREATE TABLE machines (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    -- blake3 of the machine token; the token itself is shown once, at enrolment.
    token_hash   TEXT NOT NULL UNIQUE,
    enrolled_by  TEXT,                 -- account id of the administrator
    enrolled_at  TEXT NOT NULL,
    last_seen    TEXT,
    capabilities TEXT,                 -- JSON: gpu, model hashes, app version
    revoked_at   TEXT
);
