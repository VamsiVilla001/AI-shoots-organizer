//! Persistent timing and lightweight resource samples for processing runs.

use crate::client::Db;
use crate::models::{ProcessingResourceSample, ProcessingRun, ProcessingStageTiming, ShootTelemetry};
use crate::{params, Result};

pub const SAMPLE_INTERVAL_SECONDS: i64 = 2;

/// Starts a run if this is the first worker to touch the shoot, then records
/// the earliest start of this stage. The partial unique index resolves races
/// between parallel workers.
pub fn mark_stage_started(conn: &mut dyn Db, shoot_id: i64, stage: &str) -> Result<i64> {
    // `INSERT OR IGNORE` became `ON CONFLICT … DO NOTHING`. The `WHERE status =
    // 'running'` is not a filter on the insert — it names the *predicate of the
    // partial unique index* `idx_processing_runs_active`, which is how Postgres
    // is told which constraint this conflict is about. Without it the statement
    // is rejected as having no matching arbiter index.
    conn.exec(
        "INSERT INTO processing_runs (shoot_id, status, started_at, cpu_metric_scope)
         VALUES ($1, 'running', $2, 'system')
         ON CONFLICT (shoot_id) WHERE status = 'running' DO NOTHING",
        params![shoot_id, telemetry_now()],
    )?;
    let run_id: i64 = super::at(
        &conn.row_one(
            "SELECT id FROM processing_runs
              WHERE shoot_id = $1 AND status = 'running'
              ORDER BY id DESC LIMIT 1",
            params![shoot_id],
        )?,
        0,
    )?;
    conn.exec(
        "INSERT INTO processing_stage_runs (processing_run_id, stage, started_at)
         VALUES ($1, $2, $3)
         ON CONFLICT (processing_run_id, stage) DO NOTHING",
        params![run_id, stage, telemetry_now()],
    )?;
    Ok(run_id)
}

/// Closes a stage after its final parallel job settles.
pub fn mark_stage_settled(conn: &mut dyn Db, shoot_id: i64, stage: &str, succeeded: bool) -> Result<()> {
    let outstanding: i64 = super::at(
        &conn.row_one(
            "SELECT COUNT(*) FROM jobs
              WHERE shoot_id = $1 AND kind = $2 AND state IN ('queued', 'running')",
            params![shoot_id, stage],
        )?,
        0,
    )?;
    if outstanding == 0 {
        let finished = telemetry_now();
        conn.exec(
            "UPDATE processing_stage_runs
                SET completed_at = COALESCE(completed_at, $3)
              WHERE processing_run_id = (
                    SELECT id FROM processing_runs
                     WHERE shoot_id = $1 AND status = 'running'
                     ORDER BY id DESC LIMIT 1
              ) AND stage = $2",
            params![shoot_id, stage, finished],
        )?;
        if stage == "scan" && succeeded {
            conn.exec(
                "UPDATE processing_runs SET scan_completed_at = COALESCE(scan_completed_at, $2)
                  WHERE shoot_id = $1 AND status = 'running'",
                params![shoot_id, finished],
            )?;
        }
    }
    Ok(())
}

/// Closes the run when no queued/running work remains. A failed finishing
/// stage still gets an end time and a failed status instead of looking active
/// forever.
pub fn finalize_if_settled(conn: &mut dyn Db, shoot_id: i64) -> Result<bool> {
    let outstanding: i64 = super::at(
        &conn.row_one(
            "SELECT COUNT(*) FROM jobs WHERE shoot_id = $1 AND state IN ('queued', 'running')",
            params![shoot_id],
        )?,
        0,
    )?;
    if outstanding > 0 {
        return Ok(false);
    }

    let failed: i64 = super::at(
        &conn.row_one(
            "SELECT COUNT(*) FROM jobs WHERE shoot_id = $1 AND state = 'failed'",
            params![shoot_id],
        )?,
        0,
    )?;
    let shoot_status: Option<String> = conn
        .row_opt("SELECT status FROM shoots WHERE id = $1", params![shoot_id])?
        .as_ref()
        .map(|row| super::at(row, 0))
        .transpose()?;
    let status = if failed > 0 || shoot_status.as_deref() == Some("failed") {
        "failed"
    } else if shoot_status.as_deref() == Some("paused") || shoot_status.as_deref() == Some("cancelled") {
        "cancelled"
    } else {
        "completed"
    };
    let changed = conn.exec(
        "UPDATE processing_runs SET status = $2, completed_at = COALESCE(completed_at, $3)
          WHERE shoot_id = $1 AND status = 'running'",
        params![shoot_id, status, telemetry_now()],
    )?;
    Ok(changed > 0)
}

pub fn cancel_active(conn: &mut dyn Db, shoot_id: i64) -> Result<()> {
    conn.exec(
        "UPDATE processing_runs SET status = 'cancelled', completed_at = COALESCE(completed_at, $2)
          WHERE shoot_id = $1 AND status = 'running'",
        params![shoot_id, telemetry_now()],
    )?;
    Ok(())
}

/// Saves one already-collected machine sample against the current run.
pub fn record_sample(
    conn: &mut dyn Db,
    shoot_id: i64,
    cpu_percent: Option<f64>,
    gpu_percent: Option<f64>,
    active_workers: i64,
    concurrent_shoots: i64,
    include_just_finished: bool,
) -> Result<()> {
    // `?2 = 1` became a real boolean parameter — SQLite had no boolean type, so
    // the flag had to be compared against an integer.
    let run = conn.row_opt(
        "SELECT id, started_at, completed_at FROM processing_runs
          WHERE shoot_id = $1 AND (status = 'running' OR $2)
          ORDER BY id DESC LIMIT 1",
        params![shoot_id, include_just_finished],
    )?;
    let Some(row) = run else {
        return Ok(());
    };
    let run_id: i64 = super::at(&row, 0)?;
    let started_at: String = super::at(&row, 1)?;
    let completed_at: Option<String> = super::at(&row, 2)?;

    let recorded_at = completed_at.unwrap_or_else(telemetry_now);
    let elapsed_ms = elapsed_ms(&started_at, &recorded_at);
    conn.exec(
        "INSERT INTO processing_resource_samples
             (processing_run_id, recorded_at, elapsed_ms, cpu_percent, gpu_percent, active_workers, concurrent_shoots)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
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

pub fn latest(conn: &mut dyn Db, shoot_id: i64) -> Result<ShootTelemetry> {
    // Same `julianday` -> interval rewrite as `repo::shoots::list_summaries`:
    // the `*_at` columns hold RFC3339 text, so they are cast to `timestamptz`
    // and subtracted, and scalar `MAX(0, …)` becomes `GREATEST`.
    let run = conn
        .row_opt(
            "SELECT id, shoot_id, status, started_at, scan_completed_at, completed_at, cpu_metric_scope,
                    GREATEST(0, EXTRACT(EPOCH FROM (
                        COALESCE(completed_at::timestamptz, now()) - started_at::timestamptz
                    )) * 1000)::bigint AS duration_ms
               FROM processing_runs WHERE shoot_id = $1
              ORDER BY started_at DESC, id DESC LIMIT 1",
            params![shoot_id],
        )?
        .as_ref()
        .map(|row| -> Result<ProcessingRun> {
            Ok(ProcessingRun {
                id: super::at(row, 0)?,
                shoot_id: super::at(row, 1)?,
                status: super::at(row, 2)?,
                started_at: super::at(row, 3)?,
                scan_completed_at: super::at(row, 4)?,
                completed_at: super::at(row, 5)?,
                cpu_metric_scope: super::at(row, 6)?,
                duration_ms: super::at(row, 7)?,
            })
        })
        .transpose()?;

    let Some(ref current) = run else {
        return Ok(ShootTelemetry {
            sample_interval_seconds: SAMPLE_INTERVAL_SECONDS,
            ..ShootTelemetry::default()
        });
    };

    let stages = conn
        .rows(
            "SELECT stage, started_at, completed_at FROM processing_stage_runs
              WHERE processing_run_id = $1 ORDER BY started_at, id",
            params![current.id],
        )?
        .iter()
        .map(|row| {
            Ok(ProcessingStageTiming {
                stage: super::at(row, 0)?,
                started_at: super::at(row, 1)?,
                completed_at: super::at(row, 2)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let samples = conn
        .rows(
            "SELECT recorded_at, elapsed_ms, cpu_percent, gpu_percent, active_workers, concurrent_shoots
               FROM processing_resource_samples WHERE processing_run_id = $1
              ORDER BY elapsed_ms, id",
            params![current.id],
        )?
        .iter()
        .map(|row| {
            Ok(ProcessingResourceSample {
                recorded_at: super::at(row, 0)?,
                elapsed_ms: super::at(row, 1)?,
                cpu_percent: super::at(row, 2)?,
                gpu_percent: super::at(row, 3)?,
                active_workers: super::at(row, 4)?,
                concurrent_shoots: super::at(row, 5)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

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
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "Telemetry", "C:/media").unwrap();
        let one = jobs::enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, None, 100, None).unwrap();
        let two = jobs::enqueue(&mut conn, shoot.id, JobKind::AnalyseVideo, None, 100, None).unwrap();

        mark_stage_started(&mut conn, shoot.id, JobKind::AnalyseVideo.as_str()).unwrap();
        mark_stage_started(&mut conn, shoot.id, JobKind::AnalyseVideo.as_str()).unwrap();
        jobs::complete(&mut conn, one).unwrap();
        mark_stage_settled(&mut conn, shoot.id, JobKind::AnalyseVideo.as_str(), true).unwrap();
        assert!(latest(&mut conn, shoot.id).unwrap().stages[0].completed_at.is_none());

        jobs::complete(&mut conn, two).unwrap();
        mark_stage_settled(&mut conn, shoot.id, JobKind::AnalyseVideo.as_str(), true).unwrap();
        record_sample(&mut conn, shoot.id, Some(42.0), Some(73.0), 0, 1, false).unwrap();
        assert!(finalize_if_settled(&mut conn, shoot.id).unwrap());

        let history = latest(&mut conn, shoot.id).unwrap();
        assert_eq!(history.run.unwrap().status, "completed");
        assert!(history.stages[0].completed_at.is_some());
        assert_eq!(history.samples[0].gpu_percent, Some(73.0));
    }

    /// The partial unique index is what makes two workers starting the same
    /// stage a no-op rather than a second run. `ON CONFLICT (shoot_id) WHERE
    /// status = 'running'` is how that index is named to Postgres.
    #[test]
    fn a_second_worker_joins_the_run_rather_than_starting_another() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "Telemetry", "C:/media").unwrap();

        let first = mark_stage_started(&mut conn, shoot.id, "analysePhoto").unwrap();
        let second = mark_stage_started(&mut conn, shoot.id, "analyseVideo").unwrap();
        assert_eq!(first, second, "both workers share one run");

        let runs: i64 = conn
            .row_one(
                "SELECT COUNT(*) FROM processing_runs WHERE shoot_id = $1",
                params![shoot.id],
            )
            .unwrap()
            .get(0);
        assert_eq!(runs, 1);
    }
}
