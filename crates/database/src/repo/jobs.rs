//! The resumable job queue behind §18.
//!
//! Jobs live in SQLite rather than memory so that closing the application mid
//! import leaves the work recoverable: on the next launch anything stuck in
//! `running` is returned to `queued` and picked up again.

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{Job, JobKind, JobState, ProcessingProgress};
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
    let state = if attempts < MAX_ATTEMPTS { JobState::Queued } else { JobState::Failed };
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
    let mut stmt = conn.prepare(
        "SELECT * FROM jobs WHERE shoot_id = ?1 AND state = 'failed' ORDER BY finished_at DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![shoot_id, limit], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The numbers rendered in the progress panel.
pub fn progress(conn: &Connection, shoot_id: i64) -> Result<ProcessingProgress> {
    let mut p = ProcessingProgress { shoot_id, ..Default::default() };

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

    conn.query_row(
        "SELECT SUM(CASE WHEN state = 'queued'  THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'running' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'failed'  THEN 1 ELSE 0 END)
           FROM jobs WHERE shoot_id = ?1",
        params![shoot_id],
        |r| {
            p.jobs_queued = r.get::<_, Option<i64>>(0)?.unwrap_or(0);
            p.jobs_running = r.get::<_, Option<i64>>(1)?.unwrap_or(0);
            p.jobs_failed = r.get::<_, Option<i64>>(2)?.unwrap_or(0);
            Ok(())
        },
    )?;

    // Percentage is measured in media files rather than jobs: job counts move
    // as new work is discovered, which would make the bar travel backwards.
    p.percent = if p.media_total > 0 {
        ((p.media_analysed + p.media_failed) as f64 / p.media_total as f64) * 100.0
    } else {
        0.0
    };

    p.stage = if p.jobs_running == 0 && p.jobs_queued == 0 {
        if p.media_total == 0 { "idle" } else { "complete" }
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

        assert!(enqueue_unique(&conn, shoot.id, JobKind::Cluster, None, 400).unwrap().is_some());
        assert!(enqueue_unique(&conn, shoot.id, JobKind::Cluster, None, 400).unwrap().is_none());

        let job = claim_next(&conn, None).unwrap().unwrap();
        complete(&conn, job.id).unwrap();
        assert!(enqueue_unique(&conn, shoot.id, JobKind::Cluster, None, 400).unwrap().is_some());
    }
}
