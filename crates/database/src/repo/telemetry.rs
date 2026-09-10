//! Persistent timing and lightweight resource samples for processing runs.

use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{ProcessingResourceSample, ProcessingRun, ProcessingStageTiming, ShootTelemetry};
use crate::Result;

pub const SAMPLE_INTERVAL_SECONDS: i64 = 2;

/// Starts a run if this is the first worker to touch the shoot, then records
/// the earliest start of this stage. The partial unique index resolves races
/// between parallel workers.
pub fn mark_stage_started(conn: &Connection, shoot_id: i64, stage: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO processing_runs (shoot_id, status, started_at, cpu_metric_scope)
         VALUES (?1, 'running', ?2, 'system')",
        params![shoot_id, telemetry_now()],
    )?;
    let run_id: i64 = conn.query_row(
        "SELECT id FROM processing_runs
          WHERE shoot_id = ?1 AND status = 'running'
          ORDER BY id DESC LIMIT 1",
        params![shoot_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO processing_stage_runs (processing_run_id, stage, started_at)
         VALUES (?1, ?2, ?3)",
        params![run_id, stage, telemetry_now()],
    )?;
    Ok(run_id)
}

/// Closes a stage after its final parallel job settles.
pub fn mark_stage_settled(conn: &Connection, shoot_id: i64, stage: &str, succeeded: bool) -> Result<()> {
    let outstanding: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
          WHERE shoot_id = ?1 AND kind = ?2 AND state IN ('queued', 'running')",
        params![shoot_id, stage],
        |row| row.get(0),
    )?;
    if outstanding == 0 {
        let finished = telemetry_now();
        conn.execute(
            "UPDATE processing_stage_runs
                SET completed_at = COALESCE(completed_at, ?3)
              WHERE processing_run_id = (
                    SELECT id FROM processing_runs
                     WHERE shoot_id = ?1 AND status = 'running'
                     ORDER BY id DESC LIMIT 1
              ) AND stage = ?2",
            params![shoot_id, stage, finished],
        )?;
        if stage == "scan" && succeeded {
            conn.execute(
                "UPDATE processing_runs SET scan_completed_at = COALESCE(scan_completed_at, ?2)
                  WHERE shoot_id = ?1 AND status = 'running'",
                params![shoot_id, finished],
            )?;
        }
    }
    Ok(())
}

/// Closes the run when no queued/running work remains. A failed finishing
/// stage still gets an end time and a failed status instead of looking active
/// forever.
pub fn finalize_if_settled(conn: &Connection, shoot_id: i64) -> Result<bool> {
    let outstanding: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE shoot_id = ?1 AND state IN ('queued', 'running')",
        params![shoot_id],
        |row| row.get(0),
    )?;
    if outstanding > 0 {
        return Ok(false);
    }

    let failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE shoot_id = ?1 AND state = 'failed'",
        params![shoot_id],
        |row| row.get(0),
    )?;
    let shoot_status: Option<String> = conn
        .query_row("SELECT status FROM shoots WHERE id = ?1", params![shoot_id], |row| {
            row.get(0)
        })
        .optional()?;
    let status = if failed > 0 || shoot_status.as_deref() == Some("failed") {
        "failed"
    } else if shoot_status.as_deref() == Some("paused") || shoot_status.as_deref() == Some("cancelled") {
        "cancelled"
    } else {
        "completed"
    };
    let changed = conn.execute(
        "UPDATE processing_runs SET status = ?2, completed_at = COALESCE(completed_at, ?3)
          WHERE shoot_id = ?1 AND status = 'running'",
        params![shoot_id, status, telemetry_now()],
    )?;
    Ok(changed > 0)
}

pub fn cancel_active(conn: &Connection, shoot_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE processing_runs SET status = 'cancelled', completed_at = COALESCE(completed_at, ?2)
          WHERE shoot_id = ?1 AND status = 'running'",
        params![shoot_id, telemetry_now()],
    )?;
    Ok(())
}

/// Saves one already-collected machine sample against the current run.
pub fn record_sample(
    conn: &Connection,
    shoot_id: i64,
    cpu_percent: Option<f64>,
    gpu_percent: Option<f64>,
    active_workers: i64,
    concurrent_shoots: i64,
    include_just_finished: bool,
) -> Result<()> {
    let run: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT id, started_at, completed_at FROM processing_runs
              WHERE shoot_id = ?1 AND (status = 'running' OR ?2 = 1)
              ORDER BY id DESC LIMIT 1",
            params![shoot_id, include_just_finished],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((run_id, started_at, completed_at)) = run else {
        return Ok(());
    };
    let recorded_at = completed_at.unwrap_or_else(telemetry_now);
    let elapsed_ms = elapsed_ms(&started_at, &recorded_at);
    conn.execute(
        "INSERT INTO processing_resource_samples
             (processing_run_id, recorded_at, elapsed_ms, cpu_percent, gpu_percent, active_workers, concurrent_shoots)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            run_id,
            recorded_at,
            elapsed_ms,
            cpu_percent,
            gpu_percent,
            active_workers,
            concurrent_shoots.max(1)
        ],
    )?;
    Ok(())
}

pub fn latest(conn: &Connection, shoot_id: i64) -> Result<ShootTelemetry> {
    let run = conn
        .query_row(
            "SELECT id, shoot_id, status, started_at, scan_completed_at, completed_at, cpu_metric_scope,
                    CAST(MAX(0, (julianday(COALESCE(completed_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))) -
                                 julianday(started_at)) * 86400000) AS INTEGER) AS duration_ms
               FROM processing_runs WHERE shoot_id = ?1
              ORDER BY started_at DESC, id DESC LIMIT 1",
            params![shoot_id],
            |row| {
                Ok(ProcessingRun {
                    id: row.get(0)?,
                    shoot_id: row.get(1)?,
                    status: row.get(2)?,
                    started_at: row.get(3)?,
                    scan_completed_at: row.get(4)?,
                    completed_at: row.get(5)?,
                    cpu_metric_scope: row.get(6)?,
                    duration_ms: row.get(7)?,
                })
            },
        )
        .optional()?;

    let Some(ref current) = run else {
        return Ok(ShootTelemetry {
            sample_interval_seconds: SAMPLE_INTERVAL_SECONDS,
            ..ShootTelemetry::default()
        });
    };

    let mut stage_stmt = conn.prepare(
        "SELECT stage, started_at, completed_at FROM processing_stage_runs
          WHERE processing_run_id = ?1 ORDER BY started_at, id",
    )?;
    let stages = stage_stmt
        .query_map(params![current.id], |row| {
            Ok(ProcessingStageTiming {
                stage: row.get(0)?,
                started_at: row.get(1)?,
                completed_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut sample_stmt = conn.prepare(
        "SELECT recorded_at, elapsed_ms, cpu_percent, gpu_percent, active_workers, concurrent_shoots
           FROM processing_resource_samples WHERE processing_run_id = ?1
          ORDER BY elapsed_ms, id",
    )?;
    let samples = sample_stmt
        .query_map(params![current.id], |row| {
            Ok(ProcessingResourceSample {
                recorded_at: row.get(0)?,
                elapsed_ms: row.get(1)?,
                cpu_percent: row.get(2)?,
                gpu_percent: row.get(3)?,
                active_workers: row.get(4)?,
                concurrent_shoots: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(ShootTelemetry {
        run,
        stages,
        samples,
        sample_interval_seconds: SAMPLE_INTERVAL_SECONDS,
    })
}

fn elapsed_ms(started_at: &str, recorded_at: &str) -> i64 {
    let start = chrono::DateTime::parse_from_rfc3339(started_at);
    let end = chrono::DateTime::parse_from_rfc3339(recorded_at);
    match (start, end) {
        (Ok(start), Ok(end)) => (end - start).num_milliseconds().max(0),
        _ => 0,
    }
}

fn telemetry_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::JobKind;
    use crate::repo::{jobs, shoots};
    use crate::Database;

    #[test]
    fn records_parallel_stage_once_and_keeps_completed_history() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "Telemetry", "C:/media").unwrap();
        let one = jobs::enqueue(&conn, shoot.id, JobKind::AnalyseVideo, None, 100, None).unwrap();
        let two = jobs::enqueue(&conn, shoot.id, JobKind::AnalyseVideo, None, 100, None).unwrap();

        mark_stage_started(&conn, shoot.id, JobKind::AnalyseVideo.as_str()).unwrap();
        mark_stage_started(&conn, shoot.id, JobKind::AnalyseVideo.as_str()).unwrap();
        jobs::complete(&conn, one).unwrap();
        mark_stage_settled(&conn, shoot.id, JobKind::AnalyseVideo.as_str(), true).unwrap();
        assert!(latest(&conn, shoot.id).unwrap().stages[0].completed_at.is_none());

        jobs::complete(&conn, two).unwrap();
        mark_stage_settled(&conn, shoot.id, JobKind::AnalyseVideo.as_str(), true).unwrap();
        record_sample(&conn, shoot.id, Some(42.0), Some(73.0), 0, 1, false).unwrap();
        assert!(finalize_if_settled(&conn, shoot.id).unwrap());

        let history = latest(&conn, shoot.id).unwrap();
        assert_eq!(history.run.unwrap().status, "completed");
        assert!(history.stages[0].completed_at.is_some());
        assert_eq!(history.samples[0].gpu_percent, Some(73.0));
    }
}
