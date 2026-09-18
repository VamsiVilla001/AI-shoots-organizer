//! The resumable job queue behind §18.
//!
//! Jobs live in the database rather than memory so that closing the application
//! mid import leaves the work recoverable: anything a worker held when it died
//! goes back to `queued` once its lease expires and is picked up again.
//!
//! ## `FOR UPDATE SKIP LOCKED`
//!
//! Every claim below ends in `FOR UPDATE SKIP LOCKED`, which the SQLite version
//! had no need for and no way to express. SQLite serialised writers behind one
//! lock, so `UPDATE … WHERE id = (SELECT … LIMIT 1) RETURNING *` was atomic by
//! construction. Postgres runs the workers genuinely concurrently: without the
//! locking clause two workers evaluate the same sub-select, both pick the same
//! row, and the second silently re-claims a job the first is already running —
//! which is exactly the double-analysis the `busy.media_id` guards exist to
//! prevent. `SKIP LOCKED` makes the loser move to the next candidate instead of
//! blocking, so the lanes keep their throughput.
//!
//! ## Leases and fencing
//!
//! A claim is a *lease*: the row records who holds it (`owner`), a fencing
//! token issued for that claim (`lease_token`) and when it lapses
//! (`lease_expires_at`). The holder extends it with [`heartbeat`]; a
//! [`reap_expired`] pass returns lapsed leases to the queue; and every write
//! the holder makes — [`complete`], [`fail`], [`release`], the heartbeat itself
//! — is gated on the token. Zero rows matched means the lease was already
//! reaped and someone else may now hold the job: the caller discards its work
//! rather than writing over the newer claim's.
//!
//! This replaced a startup-time `requeue_stale` of *every* running row, which
//! was correct while one process owned the queue and is a data-loss bug the
//! moment two do. It also matters on one machine: a lease that lapses hands its
//! attempt back, so only a genuine failure consumes one of the retries.

use std::time::Duration;

use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{ActiveJob, Job, JobKind, JobState, ProcessingProgress, StageProgress};
use crate::{now, params, Result};

/// A job is abandoned after this many failed attempts, so one corrupt file
/// cannot spin the workers forever.
pub const MAX_ATTEMPTS: i64 = 3;

/// How long a claim stays valid without a heartbeat. Long enough to ride out a
/// network blip or a paused laptop, short enough that a dead worker's jobs are
/// back in the queue before anyone notices the shoot has stalled.
pub const LEASE_TTL: Duration = Duration::from_secs(90);

/// How often a holder should heartbeat. Several beats fit inside one TTL, so a
/// single missed one costs nothing.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// A job whose lease lapses this many times is failed rather than requeued
/// again. Without this cap a poison job — a file that reliably exhausts GPU
/// memory and kills its worker — would cycle forever, taking a worker down each
/// time; with it, the job lands in the failed list after three.
pub const MAX_LEASE_LOSSES: i64 = 3;

/// The lease expiry for a claim made now, in the same RFC3339 UTC form as
/// every `*_at` column. Written and compared server-side only, so there is no
/// clock-skew exposure between machines.
pub fn lease_expiry_from_now() -> String {
    lease_expiry_from(chrono::Utc::now())
}

fn lease_expiry_from(at: chrono::DateTime<chrono::Utc>) -> String {
    (at + chrono::Duration::from_std(LEASE_TTL).expect("the lease TTL fits in a chrono duration"))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn map(row: &Row) -> Result<Job> {
    Ok(Job {
        id: get(row, "id")?,
        shoot_id: get(row, "shoot_id")?,
        media_id: get(row, "media_id")?,
        kind: get(row, "kind")?,
        state: get(row, "state")?,
        priority: get(row, "priority")?,
        attempts: get(row, "attempts")?,
        payload: get(row, "payload")?,
        error: get(row, "error")?,
        created_at: get(row, "created_at")?,
        started_at: get(row, "started_at")?,
        finished_at: get(row, "finished_at")?,
        owner: get(row, "owner")?,
        lease_token: get(row, "lease_token")?,
        lease_expires_at: get(row, "lease_expires_at")?,
        lease_losses: get(row, "lease_losses")?,
    })
}

fn map_opt(row: Option<Row>) -> Result<Option<Job>> {
    row.as_ref().map(map).transpose()
}

pub fn enqueue(
    conn: &mut dyn Db,
    shoot_id: i64,
    kind: JobKind,
    media_id: Option<i64>,
    priority: i64,
    payload: Option<&str>,
) -> Result<i64> {
    let row = conn.row_one(
        "INSERT INTO jobs (shoot_id, media_id, kind, priority, payload, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
        params![shoot_id, media_id, kind.as_str(), priority, payload, now()],
    )?;
    get(&row, "id")
}

/// Enqueues only if an equivalent job is not already waiting or running, so
/// re-triggering processing does not pile up duplicates.
pub fn enqueue_unique(
    conn: &mut dyn Db,
    shoot_id: i64,
    kind: JobKind,
    media_id: Option<i64>,
    priority: i64,
) -> Result<Option<i64>> {
    let existing = conn.row_opt(
        "SELECT id FROM jobs
          WHERE shoot_id = $1 AND kind = $2 AND state IN ('queued','running')
            AND ((media_id IS NULL AND $3::bigint IS NULL) OR media_id = $3::bigint)
          LIMIT 1",
        params![shoot_id, kind.as_str(), media_id],
    )?;
    if existing.is_some() {
        return Ok(None);
    }
    Ok(Some(enqueue(conn, shoot_id, kind, media_id, priority, None)?))
}

/// The `SET` clause every claim shares: the state change plus the lease. The
/// token comes from Postgres itself so it is unique without this crate needing
/// a UUID dependency. `$1` is `started_at`, `$2` the owner, `$3` the expiry.
const CLAIM_SET: &str = "state = 'running', started_at = $1, attempts = attempts + 1,
                         owner = $2, lease_token = gen_random_uuid()::text, lease_expires_at = $3";

/// Atomically claims the next queued job for `owner`. Returns `None` when the
/// queue for this shoot (or all shoots, when `shoot_id` is `None`) is empty.
pub fn claim_next(conn: &mut dyn Db, shoot_id: Option<i64>, owner: &str) -> Result<Option<Job>> {
    map_opt(conn.row_opt(
        &format!(
            "UPDATE jobs SET {CLAIM_SET}
              WHERE id = (
                  SELECT id FROM jobs
                   WHERE state = 'queued' AND ($4::bigint IS NULL OR shoot_id = $4::bigint)
                   ORDER BY priority ASC, id ASC LIMIT 1
                   FOR UPDATE SKIP LOCKED
              )
          RETURNING *"
        ),
        params![now(), owner, lease_expiry_from_now(), shoot_id],
    )?)
}

/// Claims only work that does not construct or run an AI engine. Additional
/// workers use this lane so scanning and thumbnails retain I/O concurrency
/// while a single worker owns the memory-hungry GPU sessions.
pub fn claim_next_io(conn: &mut dyn Db, owner: &str) -> Result<Option<Job>> {
    map_opt(conn.row_opt(
        &format!(
            "UPDATE jobs SET {CLAIM_SET}
              WHERE id = (
                  SELECT id FROM jobs
                   WHERE state = 'queued' AND kind IN ('scan', 'thumbnail', 'proxy')
                   ORDER BY priority ASC, id ASC LIMIT 1
                   FOR UPDATE SKIP LOCKED
              )
          RETURNING *"
        ),
        params![now(), owner, lease_expiry_from_now()],
    )?)
}

/// Claims AI and shoot-wide processing while leaving scans, thumbnails and proxies to
/// the I/O worker. Keeping the lanes independent lets GPU inference overlap
/// image indexing instead of waiting behind the entire thumbnail queue.
pub fn claim_next_compute(conn: &mut dyn Db, owner: &str) -> Result<Option<Job>> {
    map_opt(conn.row_opt(
        &format!(
            "UPDATE jobs SET {CLAIM_SET}
              WHERE id = (
                  SELECT id FROM jobs
                   WHERE state = 'queued' AND kind NOT IN ('scan', 'thumbnail', 'proxy')
                   ORDER BY priority ASC, id ASC LIMIT 1
                   FOR UPDATE SKIP LOCKED
              )
          RETURNING *"
        ),
        params![now(), owner, lease_expiry_from_now()],
    )?)
}

/// Each lane rotates independently across shoots. Multiple compute workers
/// share its cursor at the application level and claim distinct media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkerLane {
    All,
    Io,
    Compute,
}

/// Atomically claim the next ready shoot's head job after `last_shoot`.
/// Priority and FIFO still apply within each shoot. A pending dependency does
/// not consume attempts or block ready work belonging to another shoot.
pub fn claim_next_fair(
    conn: &mut dyn Db,
    lane: WorkerLane,
    last_shoot: Option<i64>,
    owner: &str,
) -> Result<Option<Job>> {
    claim_next_parallel(conn, lane, last_shoot, &[], owner)
}

/// Multiple compute workers may analyse distinct media in the same shoot.
/// Finishing stages are exclusive within a shoot and must wait for all media.
pub fn claim_next_parallel(
    conn: &mut dyn Db,
    lane: WorkerLane,
    last_shoot: Option<i64>,
    paused_shoots: &[i64],
    owner: &str,
) -> Result<Option<Job>> {
    let lane_filter = match lane {
        WorkerLane::All => "1 = 1",
        WorkerLane::Io => "kind IN ('scan', 'thumbnail', 'proxy')",
        WorkerLane::Compute => "kind NOT IN ('scan', 'thumbnail', 'proxy')",
    };
    // Both interpolations are application constants, never user-supplied SQL.
    //
    // `s.id <> ALL($5)` replaces `s.id NOT IN (SELECT value FROM json_each(?3))`:
    // the paused list no longer has to be marshalled through JSON because a
    // Postgres parameter can simply be an array of bigints.
    let sql = format!(
        "WITH heads AS (
            SELECT (
                SELECT candidate.id FROM jobs candidate
                 WHERE shoot_id = s.id AND state = 'queued' AND {lane_filter}
                   AND NOT EXISTS (
                       SELECT 1 FROM jobs busy
                        WHERE busy.state = 'running' AND busy.media_id = candidate.media_id
                   )
                   AND (
                       candidate.kind NOT IN ('scan', 'thumbnail', 'proxy')
                       OR NOT EXISTS (
                           SELECT 1 FROM jobs busy WHERE busy.shoot_id = s.id
                             AND busy.state = 'running' AND busy.kind IN ('scan', 'thumbnail', 'proxy')
                       )
                   )
                 ORDER BY candidate.priority, candidate.id LIMIT 1
            ) AS job_id
            FROM shoots s
            WHERE s.id <> ALL($5) AND NOT EXISTS (
                SELECT 1 FROM jobs
                 WHERE shoot_id = s.id AND state = 'running'
                   AND kind IN ('recognise', 'cluster', 'albums')
            )
        )
        UPDATE jobs SET {CLAIM_SET}
        WHERE id = (
            SELECT j.id FROM heads h JOIN jobs j ON j.id = h.job_id
            WHERE (
                j.kind NOT IN ('analysePhoto', 'analyseVideo')
                OR NOT EXISTS (
                    SELECT 1 FROM media m
                     WHERE m.id = j.media_id AND m.processing_status = 'pending'
                )
            ) AND (
                j.kind != 'proxy'
                OR NOT EXISTS (
                    SELECT 1 FROM jobs dependency
                     WHERE dependency.shoot_id = j.shoot_id
                       AND dependency.state IN ('queued', 'running')
                       AND dependency.kind IN ('scan', 'thumbnail', 'analysePhoto', 'analyseVideo')
                )
            ) AND (
                j.kind NOT IN ('recognise', 'cluster', 'albums')
                OR NOT EXISTS (
                    SELECT 1 FROM jobs dependency
                     WHERE dependency.shoot_id = j.shoot_id
                       AND dependency.state IN ('queued', 'running')
                       AND (dependency.kind IN ('scan', 'thumbnail', 'analysePhoto', 'analyseVideo')
                            OR (dependency.id != j.id AND dependency.kind IN ('recognise', 'cluster', 'albums')
                                AND dependency.priority < j.priority))
                )
            )
            ORDER BY (SELECT COUNT(*) FROM jobs running
                       WHERE running.shoot_id = j.shoot_id AND running.state = 'running'
                         AND running.kind IN ('analysePhoto', 'analyseVideo')),
                     CASE WHEN $4::bigint IS NULL OR j.shoot_id > $4::bigint THEN 0 ELSE 1 END,
                     j.shoot_id
            LIMIT 1
            FOR UPDATE OF j SKIP LOCKED
        ) RETURNING *"
    );
    map_opt(conn.row_opt(
        &sql,
        params![now(), owner, lease_expiry_from_now(), last_shoot, paused_shoots],
    )?)
}

/// What a heartbeat learned about the lease it tried to extend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heartbeat {
    /// Extended; keep working. Carries the new expiry.
    Alive { lease_expires_at: String },
    /// The shoot was cancelled under the worker. Stop, discard, do not report.
    Cancelled,
    /// The lease was reaped (or the job finished by someone else). The worker
    /// must discard whatever it computed — another holder may own the job.
    LeaseLost,
}

/// Extends the lease on `id` for the holder of `token`.
///
/// The worker's heartbeat runs on its own thread, so job duration is
/// irrelevant to the TTL: a twenty-minute video analysis heartbeats
/// throughout. A heartbeat sharing the worker thread would expire leases under
/// exactly the load they exist to survive.
pub fn heartbeat(conn: &mut dyn Db, id: i64, token: &str) -> Result<Heartbeat> {
    let expires = lease_expiry_from_now();
    let extended = conn.row_opt(
        "UPDATE jobs SET lease_expires_at = $2
          WHERE id = $1 AND lease_token = $3 AND state = 'running'
      RETURNING id",
        params![id, expires, token],
    )?;
    if extended.is_some() {
        return Ok(Heartbeat::Alive {
            lease_expires_at: expires,
        });
    }
    // Zero rows: either the shoot was cancelled (the row keeps its token so
    // this is distinguishable) or the lease was reaped and reissued.
    let cancelled = conn.row_opt(
        "SELECT 1 FROM jobs WHERE id = $1 AND lease_token = $2 AND state = 'cancelled'",
        params![id, token],
    )?;
    Ok(if cancelled.is_some() {
        Heartbeat::Cancelled
    } else {
        Heartbeat::LeaseLost
    })
}

/// Marks the job done. Returns `false` when the lease was no longer held —
/// the caller's results must then be treated as discarded, because a newer
/// holder may already be producing its own.
pub fn complete(conn: &mut dyn Db, id: i64, token: &str) -> Result<bool> {
    let n = conn.exec(
        "UPDATE jobs SET state = 'done', finished_at = $2, error = NULL,
                         owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE id = $1 AND lease_token = $3 AND state = 'running'",
        params![id, now(), token],
    )?;
    Ok(n == 1)
}

/// Records a failure. Below [`MAX_ATTEMPTS`] the job goes back to `queued` for
/// another try; past it, it stays failed and surfaces in the UI. `None` when
/// the lease was no longer held.
pub fn fail(conn: &mut dyn Db, id: i64, token: &str, error: &str) -> Result<Option<JobState>> {
    // One statement rather than a read of `attempts` followed by a write: with
    // concurrent workers the two could interleave and a job could be retried
    // past its budget.
    let row = conn.row_opt(
        "UPDATE jobs
            SET state = CASE WHEN attempts < $2 THEN 'queued' ELSE 'failed' END,
                error = $3,
                finished_at = $4,
                started_at = NULL,
                owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE id = $1 AND lease_token = $5 AND state = 'running'
      RETURNING state",
        params![id, MAX_ATTEMPTS, error, now(), token],
    )?;
    let Some(row) = row else { return Ok(None) };
    let state: String = get(&row, "state")?;
    JobState::parse(&state)
        .map(Some)
        .ok_or_else(|| crate::DbError::other(format!("unknown job state `{state}`")))
}

/// Returns a job to the queue without charging it an attempt — for work that
/// cannot run *yet* (a dependency still pending, a missing tool) rather than
/// work that failed. `false` when the lease was no longer held.
pub fn release(conn: &mut dyn Db, id: i64, token: &str) -> Result<bool> {
    let n = conn.exec(
        "UPDATE jobs SET state = 'queued', started_at = NULL, attempts = GREATEST(attempts - 1, 0),
                         owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE id = $1 AND lease_token = $2 AND state = 'running'",
        params![id, token],
    )?;
    Ok(n == 1)
}

/// What one reaper pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reaped {
    /// Leases that lapsed and went back to the queue.
    pub requeued: usize,
    /// Jobs that lapsed once too often and were failed instead.
    pub failed: usize,
}

/// Returns every job whose lease has lapsed to the queue, or fails it once it
/// has lapsed [`MAX_LEASE_LOSSES`] times. Run periodically by whichever
/// process brokers the queue.
///
/// A lapse hands the attempt back — `claim` always charged one — so only a
/// genuine [`fail`] consumes one of the [`MAX_ATTEMPTS`]. Without that, three
/// network blips would permanently fail a job that never actually failed.
pub fn reap_expired(conn: &mut dyn Db) -> Result<Reaped> {
    let cutoff = now();
    // Fail first, then requeue: the two predicates are disjoint on
    // `lease_losses`, so ordering only matters for not counting a row twice.
    let failed = conn.exec(
        "UPDATE jobs
            SET state = 'failed',
                error = 'the worker running this job stopped responding ' || (lease_losses + 1) || ' times',
                finished_at = $1, started_at = NULL,
                owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                lease_losses = lease_losses + 1
          WHERE state = 'running' AND lease_expires_at < $1
            AND lease_losses + 1 >= $2",
        params![cutoff, MAX_LEASE_LOSSES],
    )?;
    let requeued = conn.exec(
        "UPDATE jobs
            SET state = 'queued', owner = NULL, lease_token = NULL,
                lease_expires_at = NULL, started_at = NULL,
                attempts = GREATEST(attempts - 1, 0),
                lease_losses = lease_losses + 1
          WHERE state = 'running' AND lease_expires_at < $1
            AND lease_losses + 1 < $2",
        params![cutoff, MAX_LEASE_LOSSES],
    )?;
    Ok(Reaped {
        requeued: requeued as usize,
        failed: failed as usize,
    })
}

/// Returns the jobs *this* owner left `running` when it last stopped. Called
/// once at startup, scoped to the caller's own id: anything else that is
/// running belongs to another machine and is the reaper's business, not ours.
///
/// The attempt is handed back, as with a lapsed lease — an interrupted job did
/// not fail.
pub fn requeue_stale(conn: &mut dyn Db, owner: &str) -> Result<usize> {
    let n = conn.exec(
        "UPDATE jobs SET state = 'queued', started_at = NULL,
                         attempts = GREATEST(attempts - 1, 0),
                         owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE state = 'running' AND owner = $1",
        params![owner],
    )?;
    Ok(n as usize)
}

/// Retries everything that gave up, for the "Resume Processing" action.
pub fn retry_failed(conn: &mut dyn Db, shoot_id: i64) -> Result<usize> {
    let n = conn.exec(
        "UPDATE jobs SET state = 'queued', attempts = 0, lease_losses = 0, error = NULL,
                         started_at = NULL, finished_at = NULL,
                         owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE shoot_id = $1 AND state = 'failed'",
        params![shoot_id],
    )?;
    Ok(n as usize)
}

/// Cancels queued and running work. A running job keeps its token so the
/// holder's next [`heartbeat`] answers [`Heartbeat::Cancelled`] rather than
/// looking like a reaped lease.
pub fn cancel_for_shoot(conn: &mut dyn Db, shoot_id: i64) -> Result<usize> {
    let n = conn.exec(
        "UPDATE jobs SET state = 'cancelled', finished_at = $2, owner = NULL, lease_expires_at = NULL
          WHERE shoot_id = $1 AND state IN ('queued','running')",
        params![shoot_id, now()],
    )?;
    Ok(n as usize)
}

pub fn clear_finished(conn: &mut dyn Db, shoot_id: i64) -> Result<usize> {
    let n = conn.exec(
        "DELETE FROM jobs WHERE shoot_id = $1 AND state IN ('done','cancelled')",
        params![shoot_id],
    )?;
    Ok(n as usize)
}

pub fn pending_count(conn: &mut dyn Db, shoot_id: Option<i64>) -> Result<i64> {
    super::at(
        &conn.row_one(
            "SELECT COUNT(*) FROM jobs
              WHERE state IN ('queued','running') AND ($1::bigint IS NULL OR shoot_id = $1::bigint)",
            params![shoot_id],
        )?,
        0,
    )
}

pub fn list_failed(conn: &mut dyn Db, shoot_id: i64, limit: i64) -> Result<Vec<Job>> {
    conn.rows(
        "SELECT * FROM jobs WHERE shoot_id = $1 AND state = 'failed' ORDER BY finished_at DESC LIMIT $2",
        params![shoot_id, limit],
    )?
    .iter()
    .map(map)
    .collect()
}

/// How many running jobs the progress panel names. The pool is a handful of
/// threads, so this is a safety bound rather than a real limit.
const ACTIVE_JOB_LIMIT: i64 = 8;

/// Pipeline order for the per-stage breakdown — the same order the queue works
/// through, so the panel reads top to bottom as the work actually happens.
/// Proxies come last because they are deliberately the lowest priority.
const STAGE_ORDER: [JobKind; 8] = [
    JobKind::Scan,
    JobKind::Thumbnail,
    JobKind::AnalysePhoto,
    JobKind::AnalyseVideo,
    JobKind::Recognise,
    JobKind::Cluster,
    JobKind::Albums,
    JobKind::Proxy,
];

/// Counts every job of the shoot by kind and state.
///
/// Only kinds that have at least one job are returned: a photo-only shoot
/// should not show an empty "video analysis" step. Cancelled jobs are left out
/// — they are neither done nor outstanding.
pub fn stage_breakdown(conn: &mut dyn Db, shoot_id: i64) -> Result<Vec<StageProgress>> {
    let counted = conn
        .rows(
            "SELECT kind,
                    SUM(CASE WHEN state = 'queued'  THEN 1 ELSE 0 END),
                    SUM(CASE WHEN state = 'running' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN state = 'done'    THEN 1 ELSE 0 END),
                    SUM(CASE WHEN state = 'failed'  THEN 1 ELSE 0 END)
               FROM jobs WHERE shoot_id = $1 GROUP BY kind",
            params![shoot_id],
        )?
        .iter()
        .map(|r| {
            Ok(StageProgress {
                kind: super::at(r, 0)?,
                queued: super::at::<Option<i64>>(r, 1)?.unwrap_or(0),
                running: super::at::<Option<i64>>(r, 2)?.unwrap_or(0),
                done: super::at::<Option<i64>>(r, 3)?.unwrap_or(0),
                failed: super::at::<Option<i64>>(r, 4)?.unwrap_or(0),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut ordered: Vec<StageProgress> = STAGE_ORDER
        .iter()
        .filter_map(|kind| counted.iter().find(|s| s.kind == kind.as_str()).cloned())
        .filter(|s| s.total() > 0)
        .collect();
    // Anything the queue grows later still shows up rather than vanishing.
    ordered.extend(
        counted
            .into_iter()
            .filter(|s| s.total() > 0 && !STAGE_ORDER.iter().any(|kind| kind.as_str() == s.kind)),
    );
    Ok(ordered)
}

/// The jobs currently executing, newest claim last, with the file each one is
/// working on so the panel can name it.
pub fn running_jobs(conn: &mut dyn Db, shoot_id: i64, limit: i64) -> Result<Vec<ActiveJob>> {
    conn.rows(
        "SELECT j.id, j.kind, m.filename, j.started_at
           FROM jobs j LEFT JOIN media m ON m.id = j.media_id
          WHERE j.shoot_id = $1 AND j.state = 'running'
          ORDER BY j.started_at ASC, j.id ASC LIMIT $2",
        params![shoot_id, limit],
    )?
    .iter()
    .map(|r| {
        Ok(ActiveJob {
            job_id: super::at(r, 0)?,
            kind: super::at(r, 1)?,
            filename: super::at(r, 2)?,
            started_at: super::at(r, 3)?,
        })
    })
    .collect()
}

/// The numbers rendered in the progress panel.
pub fn progress(conn: &mut dyn Db, shoot_id: i64) -> Result<ProcessingProgress> {
    let mut p = ProcessingProgress {
        shoot_id,
        ..Default::default()
    };

    let row = conn.row_one(
        "SELECT COUNT(*),
                SUM(CASE WHEN processing_status != 'pending' THEN 1 ELSE 0 END),
                SUM(CASE WHEN processing_status = 'analysed'  THEN 1 ELSE 0 END),
                SUM(CASE WHEN processing_status = 'failed'    THEN 1 ELSE 0 END)
           FROM media WHERE shoot_id = $1",
        params![shoot_id],
    )?;
    p.media_total = super::at(&row, 0)?;
    p.media_scanned = super::at::<Option<i64>>(&row, 1)?.unwrap_or(0);
    p.media_analysed = super::at::<Option<i64>>(&row, 2)?.unwrap_or(0);
    p.media_failed = super::at::<Option<i64>>(&row, 3)?.unwrap_or(0);

    let row = conn.row_one(
        "SELECT SUM(CASE WHEN media_type = 'photo' THEN 1 ELSE 0 END),
                SUM(CASE WHEN media_type = 'video' THEN 1 ELSE 0 END)
           FROM media WHERE shoot_id = $1",
        params![shoot_id],
    )?;
    p.photos_total = super::at::<Option<i64>>(&row, 0)?.unwrap_or(0);
    p.videos_total = super::at::<Option<i64>>(&row, 1)?.unwrap_or(0);

    let row = conn.row_one(
        "SELECT COUNT(*),
                SUM(CASE WHEN person_id IS NOT NULL THEN 1 ELSE 0 END),
                SUM(CASE WHEN person_id IS NULL AND assignment != 'ignored' THEN 1 ELSE 0 END)
           FROM faces WHERE shoot_id = $1",
        params![shoot_id],
    )?;
    p.faces_detected = super::at(&row, 0)?;
    p.faces_recognised = super::at::<Option<i64>>(&row, 1)?.unwrap_or(0);
    p.faces_unknown = super::at::<Option<i64>>(&row, 2)?.unwrap_or(0);

    p.stages = stage_breakdown(conn, shoot_id)?;
    for stage in &p.stages {
        p.jobs_queued += stage.queued;
        p.jobs_running += stage.running;
        p.jobs_failed += stage.failed;
        p.jobs_done += stage.done;
    }
    p.active = running_jobs(conn, shoot_id, ACTIVE_JOB_LIMIT)?;

    // Percentage is measured in media files rather than jobs: job counts move
    // as new work is discovered, which would make the bar travel backwards.
    p.percent = if p.media_total > 0 {
        ((p.media_analysed + p.media_failed) as f64 / p.media_total as f64) * 100.0
    } else {
        0.0
    };

    p.stage = if p.jobs_running == 0 && p.jobs_queued == 0 {
        if p.media_total == 0 {
            "idle"
        } else {
            "complete"
        }
    } else if p.media_scanned < p.media_total {
        "scanning"
    } else if p.media_analysed < p.media_total {
        "analysing"
    } else {
        "finishing"
    }
    .to_string();

    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::shoots;
    use crate::Database;

    /// The fencing token a claim issued for `id`, for tests that only kept
    /// the job id around.
    fn token(conn: &mut dyn Db, id: i64) -> String {
        conn.row_one("SELECT lease_token FROM jobs WHERE id = $1", params![id])
            .unwrap()
            .get::<_, Option<String>>(0)
            .expect("the job should hold a lease")
    }

    /// Completes a job the test claimed earlier, through the token gate.
    fn finish(conn: &mut dyn Db, id: i64) {
        let token = token(conn, id);
        assert!(complete(conn, id, &token).unwrap(), "job {id} should still be leased");
    }

    /// Fails a job the test claimed earlier, through the token gate.
    fn fail_held(conn: &mut dyn Db, id: i64, error: &str) -> Option<JobState> {
        let token = token(conn, id);
        fail(conn, id, &token, error).unwrap()
    }

    /// Backdates a lease so the reaper sees it as lapsed, without waiting.
    fn expire_lease(conn: &mut dyn Db, id: i64) {
        conn.exec(
            "UPDATE jobs SET lease_expires_at = '2000-01-01T00:00:00Z' WHERE id = $1",
            params![id],
        )
        .unwrap();
    }

    #[test]
    fn a_claim_issues_a_lease_to_its_owner() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "S").unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();

        let job = claim_next(&mut conn, None, "studio-pc").unwrap().unwrap();
        assert_eq!(job.owner.as_deref(), Some("studio-pc"));
        assert!(job.lease_token.is_some());
        assert!(job.lease_expires_at.as_deref() > Some(now().as_str()), "expires in the future");
        assert_eq!(job.lease_losses, 0);
    }

    #[test]
    fn heartbeat_extends_a_live_lease_and_reports_cancellation() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "S").unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        let job = claim_next(&mut conn, None, "a").unwrap().unwrap();
        let token = job.token().unwrap();

        expire_lease(&mut conn, job.id);
        let beat = heartbeat(&mut conn, job.id, token).unwrap();
        let Heartbeat::Alive { lease_expires_at } = beat else {
            panic!("a live lease extends: {beat:?}")
        };
        assert!(lease_expires_at > now());

        cancel_for_shoot(&mut conn, shoot.id).unwrap();
        assert_eq!(heartbeat(&mut conn, job.id, token).unwrap(), Heartbeat::Cancelled);
    }

    /// The fencing rule: once a lease is reaped and re-issued, the old holder
    /// can neither extend it nor write results, however far it got.
    #[test]
    fn a_reaped_lease_fences_out_its_previous_holder() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "S").unwrap();
        let id = enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        let first = claim_next(&mut conn, None, "laptop").unwrap().unwrap();
        let stale = first.token().unwrap().to_string();

        expire_lease(&mut conn, id);
        assert_eq!(
            reap_expired(&mut conn).unwrap(),
            Reaped {
                requeued: 1,
                failed: 0
            }
        );
        let attempts: i64 = conn
            .row_one("SELECT attempts FROM jobs WHERE id = $1", params![id])
            .unwrap()
            .get(0);
        assert_eq!(attempts, 0, "a lapsed lease hands the attempt back");

        let second = claim_next(&mut conn, None, "studio-pc").unwrap().unwrap();
        assert_eq!(second.id, id);
        assert_ne!(second.token(), Some(stale.as_str()));

        assert_eq!(heartbeat(&mut conn, id, &stale).unwrap(), Heartbeat::LeaseLost);
        assert!(!complete(&mut conn, id, &stale).unwrap(), "stale results are refused");
        assert_eq!(fail(&mut conn, id, &stale, "boom").unwrap(), None);
        assert!(!release(&mut conn, id, &stale).unwrap());

        let state: String = conn
            .row_one("SELECT state FROM jobs WHERE id = $1", params![id])
            .unwrap()
            .get(0);
        assert_eq!(state, "running", "the newer holder's claim is untouched");
        assert!(complete(&mut conn, id, second.token().unwrap()).unwrap());
    }

    #[test]
    fn a_job_that_keeps_losing_its_lease_is_failed_not_cycled_forever() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "S").unwrap();
        let id = enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, None, 120, None).unwrap();

        for loss in 1..MAX_LEASE_LOSSES {
            claim_next(&mut conn, None, "flaky").unwrap().unwrap();
            expire_lease(&mut conn, id);
            let reaped = reap_expired(&mut conn).unwrap();
            assert_eq!((reaped.requeued, reaped.failed), (1, 0), "loss {loss} requeues");
        }
        claim_next(&mut conn, None, "flaky").unwrap().unwrap();
        expire_lease(&mut conn, id);
        let reaped = reap_expired(&mut conn).unwrap();
        assert_eq!((reaped.requeued, reaped.failed), (0, 1), "the last loss fails it");

        let failed = list_failed(&mut conn, shoot.id, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].lease_losses, MAX_LEASE_LOSSES);
        assert!(failed[0].error.as_deref().unwrap_or_default().contains("stopped responding"));
        assert!(claim_next(&mut conn, None, "flaky").unwrap().is_none());

        // "Resume processing" clears the loss count along with the attempts.
        assert_eq!(retry_failed(&mut conn, shoot.id).unwrap(), 1);
        assert_eq!(claim_next(&mut conn, None, "flaky").unwrap().unwrap().lease_losses, 0);
    }

    /// The bug this whole design exists to fix: a machine starting up must
    /// only recover *its own* interrupted work.
    #[test]
    fn startup_recovery_leaves_other_machines_jobs_alone() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "S").unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        let mine = claim_next(&mut conn, None, "laptop").unwrap().unwrap();
        let theirs = claim_next(&mut conn, None, "studio-pc").unwrap().unwrap();

        assert_eq!(requeue_stale(&mut conn, "laptop").unwrap(), 1);
        let state = |conn: &mut dyn Db, id: i64| -> String {
            conn.row_one("SELECT state FROM jobs WHERE id = $1", params![id])
                .unwrap()
                .get(0)
        };
        assert_eq!(state(&mut conn, mine.id), "queued");
        assert_eq!(state(&mut conn, theirs.id), "running", "still leased to the other machine");
    }

    /// Inserts one media row and returns its id, the shape most of these tests
    /// need before they can enqueue a per-file job.
    fn seed_media(conn: &mut dyn Db, shoot_id: i64, filename: &str, status: &str) -> i64 {
        conn.row_one(
            "INSERT INTO media (shoot_id, path, filename, media_type, extension, content_key, indexed_at, processing_status)
             VALUES ($1, $2, $2, 'video', 'mp4', $2, 'now', $3)
             RETURNING id",
            params![shoot_id, filename, status],
        )
        .unwrap()
        .get(0)
    }

    #[test]
    fn parallel_videos_in_one_shoot_keep_finishing_exclusive() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "Parallel", "P").unwrap();
        let mut video_jobs = Vec::new();
        for filename in ["a.mp4", "b.mp4"] {
            let media_id = seed_media(&mut conn, shoot.id, filename, "thumbnailed");
            video_jobs.push(enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap());
        }
        let recognise = enqueue(&mut conn, shoot.id, JobKind::Recognise, None, 300, None).unwrap();
        let cluster = enqueue(&mut conn, shoot.id, JobKind::Cluster, None, 400, None).unwrap();
        for id in &video_jobs {
            assert_eq!(
                claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
                    .unwrap()
                    .unwrap()
                    .id,
                *id
            );
        }
        assert!(claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
            .unwrap()
            .is_none());
        finish(&mut conn, video_jobs[0]);
        assert!(claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
            .unwrap()
            .is_none());
        finish(&mut conn, video_jobs[1]);
        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
                .unwrap()
                .unwrap()
                .id,
            recognise
        );
        assert!(claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
            .unwrap()
            .is_none());
        finish(&mut conn, recognise);
        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
                .unwrap()
                .unwrap()
                .id,
            cluster
        );
    }

    #[test]
    fn duplicate_media_jobs_never_run_together() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "S").unwrap();
        let media_id = seed_media(&mut conn, shoot.id, "a", "thumbnailed");
        let first = enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
                .unwrap()
                .unwrap()
                .id,
            first
        );
        assert!(claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
            .unwrap()
            .is_none());
    }

    /// The paused list used to be marshalled through `json_each`; it is now a
    /// bigint array parameter. An empty list must still match every shoot.
    #[test]
    fn paused_shoots_are_skipped_and_an_empty_list_pauses_nothing() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let paused = shoots::create(&mut conn, "Paused", "P").unwrap();
        let running = shoots::create(&mut conn, "Running", "R").unwrap();
        let held = enqueue(&mut conn, paused.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let ready = enqueue(&mut conn, running.id, JobKind::AnalyseVideo, None, 120, None).unwrap();

        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[paused.id], "test")
                .unwrap()
                .unwrap()
                .id,
            ready
        );
        assert!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, Some(running.id), &[paused.id], "test")
                .unwrap()
                .is_none(),
            "the paused shoot's job stays put"
        );
        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, Some(running.id), &[], "test")
                .unwrap()
                .unwrap()
                .id,
            held,
            "an empty paused list pauses nothing"
        );
    }

    #[test]
    fn proxy_waits_for_analysis_in_its_shoot() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "Proxy", "P").unwrap();
        let media_id = seed_media(&mut conn, shoot.id, "a.mp4", "thumbnailed");
        let analyse = enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        let proxy = enqueue(&mut conn, shoot.id, JobKind::Proxy, Some(media_id), 200, None).unwrap();

        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Compute, None, &[], "test")
                .unwrap()
                .unwrap()
                .id,
            analyse
        );
        assert!(claim_next_parallel(&mut conn, WorkerLane::Io, None, &[], "test")
            .unwrap()
            .is_none());
        finish(&mut conn, analyse);
        assert_eq!(
            claim_next_parallel(&mut conn, WorkerLane::Io, None, &[], "test")
                .unwrap()
                .unwrap()
                .id,
            proxy
        );
    }

    #[test]
    fn fair_compute_admits_new_shoot_before_old_backlog_finishes() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let old = shoots::create(&mut conn, "Quarter Finals", "C:/old").unwrap();
        let first = enqueue(&mut conn, old.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let second = enqueue(&mut conn, old.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let started = claim_next_fair(&mut conn, WorkerLane::Compute, None, "test").unwrap().unwrap();
        assert_eq!(started.id, first);
        let new = shoots::create(&mut conn, "GDR", "C:/new").unwrap();
        let gdr = enqueue(&mut conn, new.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        finish(&mut conn, first);
        let next = claim_next_fair(&mut conn, WorkerLane::Compute, Some(old.id), "test")
            .unwrap()
            .unwrap();
        assert_eq!(next.id, gdr);
        finish(&mut conn, gdr);
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, Some(new.id), "test")
                .unwrap()
                .unwrap()
                .id,
            second
        );
    }

    #[test]
    fn fair_lanes_rotate_three_shoots_preserving_local_priority_and_fifo() {
        for lane in [WorkerLane::Io, WorkerLane::Compute, WorkerLane::All] {
            let db = Database::open_test().unwrap();
            let mut conn = db.conn().unwrap();
            let kind = if matches!(lane, WorkerLane::Io) {
                JobKind::Thumbnail
            } else {
                JobKind::AnalyseVideo
            };
            let mut ids = Vec::new();
            let mut expected = Vec::new();
            for name in ["A", "B", "C"] {
                let s = shoots::create(&mut conn, name, name).unwrap();
                ids.push(s.id);
                let later = enqueue(&mut conn, s.id, kind, None, 120, None).unwrap();
                let first = enqueue(&mut conn, s.id, kind, None, 100, None).unwrap();
                let last = enqueue(&mut conn, s.id, kind, None, 120, None).unwrap();
                expected.push([first, later, last]);
            }
            let mut cursor = None;
            for turn in 0..3 {
                // One pass per turn, visiting every shoot: the lane must rotate
                // between shoots rather than draining one before moving on.
                for (shoot, jobs) in ids.iter().zip(&expected) {
                    let job = claim_next_fair(&mut conn, lane, cursor, "test").unwrap().unwrap();
                    assert_eq!(job.shoot_id, *shoot);
                    assert_eq!(job.id, jobs[turn]);
                    assert_eq!(job.attempts, 1);
                    finish(&mut conn, job.id);
                    cursor = Some(job.shoot_id);
                }
            }
            assert!(claim_next_fair(&mut conn, lane, cursor, "test").unwrap().is_none());
        }
    }

    #[test]
    fn fair_claim_skips_unindexed_shoot_without_charging_retries() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let waiting = shoots::create(&mut conn, "Waiting", "C:/waiting").unwrap();
        let media_id = seed_media(&mut conn, waiting.id, "a.mp4", "pending");
        let blocked = enqueue(&mut conn, waiting.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        let ready = shoots::create(&mut conn, "Ready", "C:/ready").unwrap();
        let runnable = enqueue(&mut conn, ready.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, None, "test")
                .unwrap()
                .unwrap()
                .id,
            runnable
        );
        assert!(claim_next_fair(&mut conn, WorkerLane::Compute, Some(ready.id), "test")
            .unwrap()
            .is_none());
        let attempts: i64 = conn
            .row_one("SELECT attempts FROM jobs WHERE id = $1", params![blocked])
            .unwrap()
            .get(0);
        assert_eq!(attempts, 0);
        conn.exec(
            "UPDATE media SET processing_status = 'thumbnailed' WHERE id = $1",
            params![media_id],
        )
        .unwrap();
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, Some(ready.id), "test")
                .unwrap()
                .unwrap()
                .id,
            blocked
        );
    }

    #[test]
    fn fair_finishing_waits_only_for_its_own_shoot() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let a = shoots::create(&mut conn, "A", "A").unwrap();
        let b = shoots::create(&mut conn, "B", "B").unwrap();
        let thumb = enqueue(&mut conn, a.id, JobKind::Thumbnail, None, 50, None).unwrap();
        let recognise_a = enqueue(&mut conn, a.id, JobKind::Recognise, None, 300, None).unwrap();
        let recognise_b = enqueue(&mut conn, b.id, JobKind::Recognise, None, 300, None).unwrap();
        let cluster_b = enqueue(&mut conn, b.id, JobKind::Cluster, None, 400, None).unwrap();
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, None, "test")
                .unwrap()
                .unwrap()
                .id,
            recognise_b
        );
        // Cannot run clustering concurrently with recognition in the same shoot.
        assert!(claim_next_fair(&mut conn, WorkerLane::Compute, None, "test").unwrap().is_none());
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Io, None, "test").unwrap().unwrap().id,
            thumb
        );
        finish(&mut conn, thumb);
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, Some(b.id), "test")
                .unwrap()
                .unwrap()
                .id,
            recognise_a
        );
        finish(&mut conn, recognise_b);
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, Some(a.id), "test")
                .unwrap()
                .unwrap()
                .id,
            cluster_b
        );
    }

    #[test]
    fn fair_cancel_retry_and_deleted_cursor_do_not_hold_other_shoots() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let a = shoots::create(&mut conn, "A", "A").unwrap();
        let b = shoots::create(&mut conn, "B", "B").unwrap();
        let first = enqueue(&mut conn, a.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let second = enqueue(&mut conn, b.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, None, "test")
                .unwrap()
                .unwrap()
                .id,
            first
        );
        fail_held(&mut conn, first, "temporary failure");
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, Some(a.id), "test")
                .unwrap()
                .unwrap()
                .id,
            second
        );
        finish(&mut conn, second);
        cancel_for_shoot(&mut conn, a.id).unwrap();
        assert!(claim_next_fair(&mut conn, WorkerLane::Compute, Some(b.id), "test")
            .unwrap()
            .is_none());
        conn.exec("DELETE FROM shoots WHERE id = $1", params![b.id]).unwrap();
        let later = enqueue(&mut conn, a.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        assert_eq!(
            claim_next_fair(&mut conn, WorkerLane::Compute, Some(b.id), "test")
                .unwrap()
                .unwrap()
                .id,
            later
        );
    }

    #[test]
    fn claim_is_exclusive_and_ordered_by_priority() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();

        enqueue(&mut conn, shoot.id, JobKind::Thumbnail, None, 200, None).unwrap();
        let urgent = enqueue(&mut conn, shoot.id, JobKind::Scan, None, 10, None).unwrap();

        let first = claim_next(&mut conn, None, "test").unwrap().unwrap();
        assert_eq!(first.id, urgent, "lower priority number runs first");
        assert_eq!(first.state, "running");
        assert_eq!(first.attempts, 1);

        let second = claim_next(&mut conn, None, "test").unwrap().unwrap();
        assert_ne!(second.id, first.id, "a running job cannot be claimed twice");
        assert!(claim_next(&mut conn, None, "test").unwrap().is_none());
    }

    #[test]
    fn io_claims_leave_ai_jobs_for_the_engine_worker() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();

        let analyse = enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 10, None).unwrap();
        let thumbnail = enqueue(&mut conn, shoot.id, JobKind::Thumbnail, None, 50, None).unwrap();

        assert_eq!(claim_next_io(&mut conn, "test").unwrap().unwrap().id, thumbnail);
        assert_eq!(claim_next(&mut conn, None, "test").unwrap().unwrap().id, analyse);
    }

    #[test]
    fn compute_claims_leave_io_jobs_for_the_helper_worker() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();

        let thumbnail = enqueue(&mut conn, shoot.id, JobKind::Thumbnail, None, 10, None).unwrap();
        let analyse = enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 50, None).unwrap();

        assert_eq!(claim_next_compute(&mut conn, "test").unwrap().unwrap().id, analyse);
        assert_eq!(claim_next(&mut conn, None, "test").unwrap().unwrap().id, thumbnail);
    }

    #[test]
    fn failures_retry_then_give_up() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        let id = enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();

        for _ in 0..(MAX_ATTEMPTS - 1) {
            claim_next(&mut conn, None, "test").unwrap().unwrap();
            assert_eq!(fail_held(&mut conn, id, "boom"), Some(JobState::Queued));
        }
        claim_next(&mut conn, None, "test").unwrap().unwrap();
        assert_eq!(fail_held(&mut conn, id, "boom"), Some(JobState::Failed));
        assert!(claim_next(&mut conn, None, "test").unwrap().is_none());

        assert_eq!(retry_failed(&mut conn, shoot.id).unwrap(), 1);
        assert!(claim_next(&mut conn, None, "test").unwrap().is_some());
    }

    #[test]
    fn stale_running_jobs_are_recovered_at_startup() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        claim_next(&mut conn, None, "test").unwrap().unwrap(); // simulates a crash mid-job

        assert!(claim_next(&mut conn, None, "test").unwrap().is_none());
        assert_eq!(requeue_stale(&mut conn, "test").unwrap(), 1);
        assert!(claim_next(&mut conn, None, "test").unwrap().is_some());
    }

    #[test]
    fn enqueue_unique_suppresses_duplicates() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();

        assert!(enqueue_unique(&mut conn, shoot.id, JobKind::Cluster, None, 400)
            .unwrap()
            .is_some());
        assert!(enqueue_unique(&mut conn, shoot.id, JobKind::Cluster, None, 400)
            .unwrap()
            .is_none());

        let job = claim_next(&mut conn, None, "test").unwrap().unwrap();
        finish(&mut conn, job.id);
        assert!(enqueue_unique(&mut conn, shoot.id, JobKind::Cluster, None, 400)
            .unwrap()
            .is_some());
    }

    #[test]
    fn the_breakdown_separates_finished_running_and_waiting_work() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();

        // Queued out of pipeline order to prove the breakdown re-orders them.
        enqueue(&mut conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();
        let scan = enqueue(&mut conn, shoot.id, JobKind::Scan, None, 10, None).unwrap();
        for _ in 0..3 {
            enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        }
        assert_eq!(claim_next_io(&mut conn, "test").unwrap().unwrap().id, scan);
        finish(&mut conn, scan);
        let running = claim_next_compute(&mut conn, "test").unwrap().unwrap();
        assert_eq!(running.kind, JobKind::AnalysePhoto.as_str());

        let stages = stage_breakdown(&mut conn, shoot.id).unwrap();
        let kinds: Vec<&str> = stages.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["scan", "analysePhoto", "albums"], "pipeline order");

        assert_eq!(stages[0].done, 1);
        assert_eq!((stages[1].done, stages[1].running, stages[1].queued), (0, 1, 2));
        assert_eq!(stages[2].queued, 1);

        // The panel names the file each running job is working on.
        let active = running_jobs(&mut conn, shoot.id, 8).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].job_id, running.id);
        assert_eq!(active[0].filename, None, "shoot-wide jobs have no file");

        let progress = progress(&mut conn, shoot.id).unwrap();
        assert_eq!(progress.jobs_done, 1);
        assert_eq!(progress.jobs_running, 1);
        assert_eq!(progress.jobs_queued, 3);
    }

    #[test]
    fn a_cancelled_shoot_leaves_no_outstanding_steps() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        cancel_for_shoot(&mut conn, shoot.id).unwrap();

        // Cancelled work is neither done nor pending, so it drops out entirely
        // rather than sitting in the panel as a step that never finishes.
        assert!(stage_breakdown(&mut conn, shoot.id).unwrap().is_empty());
    }
}
