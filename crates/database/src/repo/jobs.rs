//! The resumable job queue behind §18.
//!
//! Jobs live in SQLite rather than memory so that closing the application mid
//! import leaves the work recoverable: on the next launch anything stuck in
//! `running` is returned to `queued` and picked up again.

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{ActiveJob, Job, JobKind, JobState, ProcessingProgress, StageProgress};
use crate::{now, Result};

/// A job is abandoned after this many failed attempts, so one corrupt file
/// cannot spin the workers forever.
pub const MAX_ATTEMPTS: i64 = 3;

fn map(row: &Row<'_>) -> rusqlite::Result<Job> {
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
    })
}

pub fn enqueue(
    conn: &Connection,
    shoot_id: i64,
    kind: JobKind,
    media_id: Option<i64>,
    priority: i64,
    payload: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO jobs (shoot_id, media_id, kind, priority, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![shoot_id, media_id, kind, priority, payload, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Enqueues only if an equivalent job is not already waiting or running, so
/// re-triggering processing does not pile up duplicates.
pub fn enqueue_unique(
    conn: &Connection,
    shoot_id: i64,
    kind: JobKind,
    media_id: Option<i64>,
    priority: i64,
) -> Result<Option<i64>> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM jobs
              WHERE shoot_id = ?1 AND kind = ?2 AND state IN ('queued','running')
                AND ((media_id IS NULL AND ?3 IS NULL) OR media_id = ?3)
              LIMIT 1",
            params![shoot_id, kind, media_id],
            |r| r.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(None);
    }
    Ok(Some(enqueue(conn, shoot_id, kind, media_id, priority, None)?))
}

/// Atomically claims the next queued job. Returns `None` when the queue for
/// this shoot (or all shoots, when `shoot_id` is `None`) is empty.
///
/// The claim is a single `UPDATE ... RETURNING` so two workers racing on the
/// same row cannot both win it.
pub fn claim_next(conn: &Connection, shoot_id: Option<i64>) -> Result<Option<Job>> {
    let job = conn
        .prepare(
            "UPDATE jobs SET state = 'running', started_at = ?1, attempts = attempts + 1
              WHERE id = (
                  SELECT id FROM jobs
                   WHERE state = 'queued' AND (?2 IS NULL OR shoot_id = ?2)
                   ORDER BY priority ASC, id ASC LIMIT 1
              )
          RETURNING *",
        )?
        .query_row(params![now(), shoot_id], map)
        .optional()?;
    Ok(job)
}

/// Claims only work that does not construct or run an AI engine. Additional
/// workers use this lane so scanning and thumbnails retain I/O concurrency
/// while a single worker owns the memory-hungry GPU sessions.
pub fn claim_next_io(conn: &Connection) -> Result<Option<Job>> {
    let job = conn
        .prepare(
            "UPDATE jobs SET state = 'running', started_at = ?1, attempts = attempts + 1
              WHERE id = (
                  SELECT id FROM jobs
                   WHERE state = 'queued' AND kind IN ('scan', 'thumbnail', 'proxy')
                   ORDER BY priority ASC, id ASC LIMIT 1
              )
          RETURNING *",
        )?
        .query_row(params![now()], map)
        .optional()?;
    Ok(job)
}

/// Claims AI and shoot-wide processing while leaving scans, thumbnails and proxies to
/// the I/O worker. Keeping the lanes independent lets GPU inference overlap
/// image indexing instead of waiting behind the entire thumbnail queue.
pub fn claim_next_compute(conn: &Connection) -> Result<Option<Job>> {
    let job = conn
        .prepare(
            "UPDATE jobs SET state = 'running', started_at = ?1, attempts = attempts + 1
              WHERE id = (
                  SELECT id FROM jobs
                   WHERE state = 'queued' AND kind NOT IN ('scan', 'thumbnail', 'proxy')
                   ORDER BY priority ASC, id ASC LIMIT 1
              )
          RETURNING *",
        )?
        .query_row(params![now()], map)
        .optional()?;
    Ok(job)
}

/// Each lane rotates independently across shoots. Multiple compute workers
/// share its cursor at the application level and claim distinct media.
#[derive(Debug, Clone, Copy)]
pub enum WorkerLane {
    All,
    Io,
    Compute,
}

/// Atomically claim the next ready shoot's head job after `last_shoot`.
/// Priority and FIFO still apply within each shoot. A pending dependency does
/// not consume attempts or block ready work belonging to another shoot.
pub fn claim_next_fair(conn: &Connection, lane: WorkerLane, last_shoot: Option<i64>) -> Result<Option<Job>> {
    claim_next_parallel(conn, lane, last_shoot, &[])
}

/// Multiple compute workers may analyse distinct media in the same shoot.
/// Finishing stages are exclusive within a shoot and must wait for all media.
pub fn claim_next_parallel(
    conn: &Connection,
    lane: WorkerLane,
    last_shoot: Option<i64>,
    paused_shoots: &[i64],
) -> Result<Option<Job>> {
    let lane_filter = match lane {
        WorkerLane::All => "1 = 1",
        WorkerLane::Io => "kind IN ('scan', 'thumbnail', 'proxy')",
        WorkerLane::Compute => "kind NOT IN ('scan', 'thumbnail', 'proxy')",
    };
    // Both interpolations are application constants, never user-supplied SQL.
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
            WHERE s.id NOT IN (SELECT value FROM json_each(?3)) AND NOT EXISTS (
                SELECT 1 FROM jobs
                 WHERE shoot_id = s.id AND state = 'running'
                   AND kind IN ('recognise', 'cluster', 'albums')
            )
        )
        UPDATE jobs SET state = 'running', started_at = ?1, attempts = attempts + 1
        WHERE id = (
            SELECT j.id FROM heads h JOIN jobs j ON j.id = h.job_id
            WHERE (
                j.kind NOT IN ('analysePhoto', 'analyseVideo')
                OR NOT EXISTS (
                    SELECT 1 FROM media m
                     WHERE m.id = j.media_id AND m.processing_status = 'pending'
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
                     CASE WHEN ?2 IS NULL OR j.shoot_id > ?2 THEN 0 ELSE 1 END,
                     j.shoot_id
            LIMIT 1
        ) RETURNING *"
    );
    Ok(conn
        .prepare(&sql)?
        .query_row(params![now(), last_shoot, serde_json::to_string(paused_shoots)?], map)
        .optional()?)
}

pub fn complete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = 'done', finished_at = ?2, error = NULL WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

/// Records a failure. Below [`MAX_ATTEMPTS`] the job goes back to `queued` for
/// another try; past it, it stays failed and surfaces in the UI.
pub fn fail(conn: &Connection, id: i64, error: &str) -> Result<JobState> {
    let attempts: i64 = conn.query_row("SELECT attempts FROM jobs WHERE id = ?1", params![id], |r| r.get(0))?;
    let state = if attempts < MAX_ATTEMPTS {
        JobState::Queued
    } else {
        JobState::Failed
    };
    conn.execute(
        "UPDATE jobs SET state = ?2, error = ?3, finished_at = ?4 WHERE id = ?1",
        params![id, state, error, now()],
    )?;
    Ok(state)
}

/// Returns jobs abandoned by a previous run to the queue. Called once at
/// startup — this is what makes processing resumable across restarts.
pub fn requeue_stale(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'queued', started_at = NULL
          WHERE state = 'running' AND attempts < ?1",
        params![MAX_ATTEMPTS],
    )?)
}

/// Retries everything that gave up, for the "Resume Processing" action.
pub fn retry_failed(conn: &Connection, shoot_id: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'queued', attempts = 0, error = NULL, started_at = NULL, finished_at = NULL
          WHERE shoot_id = ?1 AND state = 'failed'",
        params![shoot_id],
    )?)
}

pub fn cancel_for_shoot(conn: &Connection, shoot_id: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'cancelled', finished_at = ?2 WHERE shoot_id = ?1 AND state IN ('queued','running')",
        params![shoot_id, now()],
    )?)
}

pub fn clear_finished(conn: &Connection, shoot_id: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM jobs WHERE shoot_id = ?1 AND state IN ('done','cancelled')",
        params![shoot_id],
    )?)
}

pub fn pending_count(conn: &Connection, shoot_id: Option<i64>) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE state IN ('queued','running') AND (?1 IS NULL OR shoot_id = ?1)",
        params![shoot_id],
        |r| r.get(0),
    )?)
}

pub fn list_failed(conn: &Connection, shoot_id: i64, limit: i64) -> Result<Vec<Job>> {
    let mut stmt =
        conn.prepare("SELECT * FROM jobs WHERE shoot_id = ?1 AND state = 'failed' ORDER BY finished_at DESC LIMIT ?2")?;
    let rows = stmt
        .query_map(params![shoot_id, limit], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
pub fn stage_breakdown(conn: &Connection, shoot_id: i64) -> Result<Vec<StageProgress>> {
    let mut stmt = conn.prepare(
        "SELECT kind,
                SUM(CASE WHEN state = 'queued'  THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'running' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'done'    THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'failed'  THEN 1 ELSE 0 END)
           FROM jobs WHERE shoot_id = ?1 GROUP BY kind",
    )?;
    let counted = stmt
        .query_map(params![shoot_id], |r| {
            Ok(StageProgress {
                kind: r.get(0)?,
                queued: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                running: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                done: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                failed: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

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
pub fn running_jobs(conn: &Connection, shoot_id: i64, limit: i64) -> Result<Vec<ActiveJob>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.kind, m.filename, j.started_at
           FROM jobs j LEFT JOIN media m ON m.id = j.media_id
          WHERE j.shoot_id = ?1 AND j.state = 'running'
          ORDER BY j.started_at ASC, j.id ASC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![shoot_id, limit], |r| {
            Ok(ActiveJob {
                job_id: r.get(0)?,
                kind: r.get(1)?,
                filename: r.get(2)?,
                started_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The numbers rendered in the progress panel.
pub fn progress(conn: &Connection, shoot_id: i64) -> Result<ProcessingProgress> {
    let mut p = ProcessingProgress {
        shoot_id,
        ..Default::default()
    };

    conn.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN processing_status != 'pending' THEN 1 ELSE 0 END),
                SUM(CASE WHEN processing_status = 'analysed'  THEN 1 ELSE 0 END),
                SUM(CASE WHEN processing_status = 'failed'    THEN 1 ELSE 0 END)
           FROM media WHERE shoot_id = ?1",
        params![shoot_id],
        |r| {
            p.media_total = r.get(0)?;
            p.media_scanned = r.get::<_, Option<i64>>(1)?.unwrap_or(0);
            p.media_analysed = r.get::<_, Option<i64>>(2)?.unwrap_or(0);
            p.media_failed = r.get::<_, Option<i64>>(3)?.unwrap_or(0);
            Ok(())
        },
    )?;

    conn.query_row(
        "SELECT SUM(CASE WHEN media_type = 'photo' THEN 1 ELSE 0 END),
                SUM(CASE WHEN media_type = 'video' THEN 1 ELSE 0 END)
           FROM media WHERE shoot_id = ?1",
        params![shoot_id],
        |r| {
            p.photos_total = r.get::<_, Option<i64>>(0)?.unwrap_or(0);
            p.videos_total = r.get::<_, Option<i64>>(1)?.unwrap_or(0);
            Ok(())
        },
    )?;

    conn.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN person_id IS NOT NULL THEN 1 ELSE 0 END),
                SUM(CASE WHEN person_id IS NULL AND assignment != 'ignored' THEN 1 ELSE 0 END)
           FROM faces WHERE shoot_id = ?1",
        params![shoot_id],
        |r| {
            p.faces_detected = r.get(0)?;
            p.faces_recognised = r.get::<_, Option<i64>>(1)?.unwrap_or(0);
            p.faces_unknown = r.get::<_, Option<i64>>(2)?.unwrap_or(0);
            Ok(())
        },
    )?;

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

    #[test]
    fn parallel_videos_in_one_shoot_keep_finishing_exclusive() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "Parallel", "P").unwrap();
        let mut video_jobs = Vec::new();
        for filename in ["a.mp4", "b.mp4"] {
            conn.execute("INSERT INTO media (shoot_id,path,filename,media_type,extension,content_key,indexed_at,processing_status) VALUES (?1,?2,?2,'video','mp4',?2,'now','thumbnailed')", params![shoot.id, filename]).unwrap();
            let media_id = conn.last_insert_rowid();
            video_jobs.push(enqueue(&conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap());
        }
        let recognise = enqueue(&conn, shoot.id, JobKind::Recognise, None, 300, None).unwrap();
        let cluster = enqueue(&conn, shoot.id, JobKind::Cluster, None, 400, None).unwrap();
        for id in &video_jobs {
            assert_eq!(
                claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
                    .unwrap()
                    .unwrap()
                    .id,
                *id
            );
        }
        assert!(claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
            .unwrap()
            .is_none());
        complete(&conn, video_jobs[0]).unwrap();
        assert!(claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
            .unwrap()
            .is_none());
        complete(&conn, video_jobs[1]).unwrap();
        assert_eq!(
            claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
                .unwrap()
                .unwrap()
                .id,
            recognise
        );
        assert!(claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
            .unwrap()
            .is_none());
        complete(&conn, recognise).unwrap();
        assert_eq!(
            claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
                .unwrap()
                .unwrap()
                .id,
            cluster
        );
    }

    #[test]
    fn duplicate_media_jobs_never_run_together() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "S").unwrap();
        conn.execute("INSERT INTO media (shoot_id,path,filename,media_type,extension,content_key,indexed_at,processing_status) VALUES (?1,'a','a','video','mp4','a','now','thumbnailed')", [shoot.id]).unwrap();
        let media_id = conn.last_insert_rowid();
        let first = enqueue(&conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        enqueue(&conn, shoot.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        assert_eq!(
            claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
                .unwrap()
                .unwrap()
                .id,
            first
        );
        assert!(claim_next_parallel(&conn, WorkerLane::Compute, None, &[])
            .unwrap()
            .is_none());
    }

    #[test]
    fn fair_compute_admits_new_shoot_before_old_backlog_finishes() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let old = shoots::create(&conn, "Quarter Finals", "C:/old").unwrap();
        let first = enqueue(&conn, old.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let second = enqueue(&conn, old.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let started = claim_next_fair(&conn, WorkerLane::Compute, None).unwrap().unwrap();
        assert_eq!(started.id, first);
        let new = shoots::create(&conn, "GDR", "C:/new").unwrap();
        let gdr = enqueue(&conn, new.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        complete(&conn, first).unwrap();
        let next = claim_next_fair(&conn, WorkerLane::Compute, Some(old.id))
            .unwrap()
            .unwrap();
        assert_eq!(next.id, gdr);
        complete(&conn, gdr).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, Some(new.id))
                .unwrap()
                .unwrap()
                .id,
            second
        );
    }

    #[test]
    fn fair_lanes_rotate_three_shoots_preserving_local_priority_and_fifo() {
        for lane in [WorkerLane::Io, WorkerLane::Compute, WorkerLane::All] {
            let db = Database::open_in_memory().unwrap();
            let conn = db.conn().unwrap();
            let kind = if matches!(lane, WorkerLane::Io) {
                JobKind::Thumbnail
            } else {
                JobKind::AnalyseVideo
            };
            let mut ids = Vec::new();
            let mut expected = Vec::new();
            for name in ["A", "B", "C"] {
                let s = shoots::create(&conn, name, name).unwrap();
                ids.push(s.id);
                let later = enqueue(&conn, s.id, kind, None, 120, None).unwrap();
                let first = enqueue(&conn, s.id, kind, None, 100, None).unwrap();
                let last = enqueue(&conn, s.id, kind, None, 120, None).unwrap();
                expected.push([first, later, last]);
            }
            let mut cursor = None;
            for turn in 0..3 {
                for (i, shoot) in ids.iter().enumerate() {
                    let job = claim_next_fair(&conn, lane, cursor).unwrap().unwrap();
                    assert_eq!(job.shoot_id, *shoot);
                    assert_eq!(job.id, expected[i][turn]);
                    assert_eq!(job.attempts, 1);
                    complete(&conn, job.id).unwrap();
                    cursor = Some(job.shoot_id);
                }
            }
            assert!(claim_next_fair(&conn, lane, cursor).unwrap().is_none());
        }
    }

    #[test]
    fn fair_claim_skips_unindexed_shoot_without_charging_retries() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let waiting = shoots::create(&conn, "Waiting", "C:/waiting").unwrap();
        conn.execute("INSERT INTO media (shoot_id, path, filename, media_type, extension, content_key, indexed_at) VALUES (?1, 'a.mp4', 'a.mp4', 'video', 'mp4', 'key', 'now')", [waiting.id]).unwrap();
        let media_id = conn.last_insert_rowid();
        let blocked = enqueue(&conn, waiting.id, JobKind::AnalyseVideo, Some(media_id), 120, None).unwrap();
        let ready = shoots::create(&conn, "Ready", "C:/ready").unwrap();
        let runnable = enqueue(&conn, ready.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, None).unwrap().unwrap().id,
            runnable
        );
        assert!(claim_next_fair(&conn, WorkerLane::Compute, Some(ready.id))
            .unwrap()
            .is_none());
        assert_eq!(
            conn.query_row("SELECT attempts FROM jobs WHERE id = ?1", [blocked], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        conn.execute(
            "UPDATE media SET processing_status = 'thumbnailed' WHERE id = ?1",
            [media_id],
        )
        .unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, Some(ready.id))
                .unwrap()
                .unwrap()
                .id,
            blocked
        );
    }

    #[test]
    fn fair_finishing_waits_only_for_its_own_shoot() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let a = shoots::create(&conn, "A", "A").unwrap();
        let b = shoots::create(&conn, "B", "B").unwrap();
        let thumb = enqueue(&conn, a.id, JobKind::Thumbnail, None, 50, None).unwrap();
        let recognise_a = enqueue(&conn, a.id, JobKind::Recognise, None, 300, None).unwrap();
        let recognise_b = enqueue(&conn, b.id, JobKind::Recognise, None, 300, None).unwrap();
        let cluster_b = enqueue(&conn, b.id, JobKind::Cluster, None, 400, None).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, None).unwrap().unwrap().id,
            recognise_b
        );
        // Cannot run clustering concurrently with recognition in the same shoot.
        assert!(claim_next_fair(&conn, WorkerLane::Compute, None).unwrap().is_none());
        assert_eq!(claim_next_fair(&conn, WorkerLane::Io, None).unwrap().unwrap().id, thumb);
        complete(&conn, thumb).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, Some(b.id))
                .unwrap()
                .unwrap()
                .id,
            recognise_a
        );
        complete(&conn, recognise_b).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, Some(a.id))
                .unwrap()
                .unwrap()
                .id,
            cluster_b
        );
    }

    #[test]
    fn fair_cancel_retry_and_deleted_cursor_do_not_hold_other_shoots() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let a = shoots::create(&conn, "A", "A").unwrap();
        let b = shoots::create(&conn, "B", "B").unwrap();
        let first = enqueue(&conn, a.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let second = enqueue(&conn, b.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, None).unwrap().unwrap().id,
            first
        );
        fail(&conn, first, "temporary failure").unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, Some(a.id))
                .unwrap()
                .unwrap()
                .id,
            second
        );
        complete(&conn, second).unwrap();
        cancel_for_shoot(&conn, a.id).unwrap();
        assert!(claim_next_fair(&conn, WorkerLane::Compute, Some(b.id))
            .unwrap()
            .is_none());
        conn.execute("DELETE FROM shoots WHERE id = ?1", [b.id]).unwrap();
        let later = enqueue(&conn, a.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        assert_eq!(
            claim_next_fair(&conn, WorkerLane::Compute, Some(b.id))
                .unwrap()
                .unwrap()
                .id,
            later
        );
    }

    #[test]
    fn claim_is_exclusive_and_ordered_by_priority() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();

        enqueue(&conn, shoot.id, JobKind::Thumbnail, None, 200, None).unwrap();
        let urgent = enqueue(&conn, shoot.id, JobKind::Scan, None, 10, None).unwrap();

        let first = claim_next(&conn, None).unwrap().unwrap();
        assert_eq!(first.id, urgent, "lower priority number runs first");
        assert_eq!(first.state, "running");
        assert_eq!(first.attempts, 1);

        let second = claim_next(&conn, None).unwrap().unwrap();
        assert_ne!(second.id, first.id, "a running job cannot be claimed twice");
        assert!(claim_next(&conn, None).unwrap().is_none());
    }

    #[test]
    fn io_claims_leave_ai_jobs_for_the_engine_worker() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();

        let analyse = enqueue(&conn, shoot.id, JobKind::AnalysePhoto, None, 10, None).unwrap();
        let thumbnail = enqueue(&conn, shoot.id, JobKind::Thumbnail, None, 50, None).unwrap();

        assert_eq!(claim_next_io(&conn).unwrap().unwrap().id, thumbnail);
        assert_eq!(claim_next(&conn, None).unwrap().unwrap().id, analyse);
    }

    #[test]
    fn compute_claims_leave_io_jobs_for_the_helper_worker() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();

        let thumbnail = enqueue(&conn, shoot.id, JobKind::Thumbnail, None, 10, None).unwrap();
        let analyse = enqueue(&conn, shoot.id, JobKind::AnalysePhoto, None, 50, None).unwrap();

        assert_eq!(claim_next_compute(&conn).unwrap().unwrap().id, analyse);
        assert_eq!(claim_next(&conn, None).unwrap().unwrap().id, thumbnail);
    }

    #[test]
    fn failures_retry_then_give_up() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();
        let id = enqueue(&conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();

        for _ in 0..(MAX_ATTEMPTS - 1) {
            claim_next(&conn, None).unwrap().unwrap();
            assert_eq!(fail(&conn, id, "boom").unwrap(), JobState::Queued);
        }
        claim_next(&conn, None).unwrap().unwrap();
        assert_eq!(fail(&conn, id, "boom").unwrap(), JobState::Failed);
        assert!(claim_next(&conn, None).unwrap().is_none());

        assert_eq!(retry_failed(&conn, shoot.id).unwrap(), 1);
        assert!(claim_next(&conn, None).unwrap().is_some());
    }

    #[test]
    fn stale_running_jobs_are_recovered_at_startup() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();
        enqueue(&conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        claim_next(&conn, None).unwrap().unwrap(); // simulates a crash mid-job

        assert!(claim_next(&conn, None).unwrap().is_none());
        assert_eq!(requeue_stale(&conn).unwrap(), 1);
        assert!(claim_next(&conn, None).unwrap().is_some());
    }

    #[test]
    fn enqueue_unique_suppresses_duplicates() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();

        assert!(enqueue_unique(&conn, shoot.id, JobKind::Cluster, None, 400)
            .unwrap()
            .is_some());
        assert!(enqueue_unique(&conn, shoot.id, JobKind::Cluster, None, 400)
            .unwrap()
            .is_none());

        let job = claim_next(&conn, None).unwrap().unwrap();
        complete(&conn, job.id).unwrap();
        assert!(enqueue_unique(&conn, shoot.id, JobKind::Cluster, None, 400)
            .unwrap()
            .is_some());
    }

    #[test]
    fn the_breakdown_separates_finished_running_and_waiting_work() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();

        // Queued out of pipeline order to prove the breakdown re-orders them.
        enqueue(&conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();
        let scan = enqueue(&conn, shoot.id, JobKind::Scan, None, 10, None).unwrap();
        for _ in 0..3 {
            enqueue(&conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        }
        complete(&conn, scan).unwrap();
        let running = claim_next_compute(&conn).unwrap().unwrap();
        assert_eq!(running.kind, JobKind::AnalysePhoto.as_str());

        let stages = stage_breakdown(&conn, shoot.id).unwrap();
        let kinds: Vec<&str> = stages.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["scan", "analysePhoto", "albums"], "pipeline order");

        assert_eq!(stages[0].done, 1);
        assert_eq!((stages[1].done, stages[1].running, stages[1].queued), (0, 1, 2));
        assert_eq!(stages[2].queued, 1);

        // The panel names the file each running job is working on.
        let active = running_jobs(&conn, shoot.id, 8).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].job_id, running.id);
        assert_eq!(active[0].filename, None, "shoot-wide jobs have no file");

        let progress = progress(&conn, shoot.id).unwrap();
        assert_eq!(progress.jobs_done, 1);
        assert_eq!(progress.jobs_running, 1);
        assert_eq!(progress.jobs_queued, 3);
    }

    #[test]
    fn a_cancelled_shoot_leaves_no_outstanding_steps() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();
        enqueue(&conn, shoot.id, JobKind::AnalysePhoto, None, 100, None).unwrap();
        cancel_for_shoot(&conn, shoot.id).unwrap();

        // Cancelled work is neither done nor pending, so it drops out entirely
        // rather than sitting in the panel as a step that never finishes.
        assert!(stage_breakdown(&conn, shoot.id).unwrap().is_empty());
    }
}
