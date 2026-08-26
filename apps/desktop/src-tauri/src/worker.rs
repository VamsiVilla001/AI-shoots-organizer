//! The background worker pool (§18).
//!
//! Workers pull jobs from the SQLite queue, so the queue survives a crash or a
//! quit: anything left `running` is returned to `queued` at startup and picked
//! up again. Nothing here blocks the UI thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use teo_database::models::{Job, JobKind, JobState, ProcessingStatus};
use teo_database::repo::{jobs, logs, media as media_repo};

use crate::events;
use crate::pipeline::{Engine, PipelineError};
use crate::stages;
use crate::state::AppState;

/// How long a worker waits before looking for work again when the queue is empty.
const IDLE_POLL: Duration = Duration::from_millis(300);

/// Native ONNX sessions hold hundreds of megabytes of model and GPU state.
/// Release them after the AI queue goes quiet instead of retaining one pair
/// per worker for the rest of the application's lifetime.
const ENGINE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the monitor pushes progress to the UI. Fast enough to feel live,
/// slow enough not to flood the IPC channel on a large import.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

pub struct WorkerPool {
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl WorkerPool {
    /// Starts `worker_threads` workers plus one progress monitor.
    pub fn start(app: AppHandle, state: Arc<AppState>) -> Self {
        // Recover anything a previous run left mid-flight.
        match state.db.conn().and_then(|conn| jobs::requeue_stale(&conn)) {
            Ok(n) if n > 0 => tracing::info!(jobs = n, "recovered interrupted jobs from the previous session"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "could not recover interrupted jobs"),
        }

        let worker_count = state.settings().worker_threads.max(1);
        let mut handles = Vec::with_capacity(worker_count + 1);

        for index in 0..worker_count {
            let app = app.clone();
            let state = Arc::clone(&state);
            let has_io_worker = worker_count > 1;
            handles.push(
                std::thread::Builder::new()
                    .name(format!("teo-worker-{index}"))
                    .spawn(move || worker_loop(index, has_io_worker, app, state))
                    .expect("failed to spawn worker thread"),
            );
        }

        let monitor_app = app.clone();
        let monitor_state = Arc::clone(&state);
        handles.push(
            std::thread::Builder::new()
                .name("teo-monitor".into())
                .spawn(move || monitor_loop(monitor_app, monitor_state))
                .expect("failed to spawn monitor thread"),
        );

        Self { handles }
    }

    /// Waits for workers to notice the shutdown flag and stop.
    pub fn join(self) {
        for handle in self.handles {
            let _ = handle.join();
        }
    }
}

fn worker_loop(index: usize, has_io_worker: bool, app: AppHandle, state: Arc<AppState>) {
    tracing::debug!(worker = index, "worker started");

    // Built on first use: a session that only ever browses an existing shoot
    // should not pay to load two ONNX models.
    let mut engine: Option<Engine> = None;
    let mut engine_last_used: Option<Instant> = None;
    let mut engine_version: u64 = 0;
    // FFmpeg is resolved per worker so thumbnail jobs never need the engine —
    // indexing works with no models installed.
    let mut tools_version = state.settings_version();
    let mut ffmpeg = crate::pipeline::discover_ffmpeg(&state.settings());

    while !state.is_shutting_down() {
        if state.is_paused() {
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        // Worker zero owns AI and finishing stages. Extra workers stay on the
        // lightweight I/O lane so only one detector/embedder pair occupies
        // RAM and GPU memory, while thumbnails can still run in parallel.
        let claimed = match state.db.conn().and_then(|conn| {
            if !has_io_worker {
                jobs::claim_next(&conn, None)
            } else if index == 0 {
                jobs::claim_next_compute(&conn)
            } else {
                jobs::claim_next_io(&conn)
            }
        }) {
            Ok(job) => job,
            Err(e) => {
                tracing::error!(worker = index, error = %e, "could not claim a job");
                std::thread::sleep(IDLE_POLL);
                continue;
            }
        };

        let Some(job) = claimed else {
            if engine.is_some() && engine_last_used.is_some_and(|used| used.elapsed() >= ENGINE_IDLE_TIMEOUT) {
                tracing::info!(worker = index, "unloading idle face models");
                engine = None;
                engine_last_used = None;
            }
            std::thread::sleep(IDLE_POLL);
            continue;
        };

        // A cancelled shoot's remaining jobs are dropped rather than run.
        if state.is_cancelled(job.shoot_id) {
            if let Ok(conn) = state.db.conn() {
                let _ = jobs::cancel_for_shoot(&conn, job.shoot_id);
            }
            continue;
        }

        // Settings changed since these were built: rebuild so new thresholds,
        // accelerator choices and the FFmpeg path apply immediately.
        if tools_version != state.settings_version() {
            if engine.is_some() {
                tracing::info!(worker = index, "settings changed; reloading models");
                engine = None;
                engine_last_used = None;
            }
            ffmpeg = crate::pipeline::discover_ffmpeg(&state.settings());
            tools_version = state.settings_version();
        }

        let outcome = run_job(&app, &state, &job, &mut engine, &mut engine_version, ffmpeg.as_ref());
        if matches!(JobKind::parse(&job.kind), Some(JobKind::AnalysePhoto | JobKind::AnalyseVideo)) {
            engine_last_used = Some(Instant::now());
        }
        finish_job(&app, &state, &job, outcome);
    }

    tracing::debug!(worker = index, "worker stopped");
}

/// A job either finishes, fails, or asks to be tried again later.
enum JobOutcome {
    Done,
    /// The job cannot run yet — put it back without counting an attempt.
    Deferred,
    /// Nothing is wrong with *this* file: the whole pipeline is missing
    /// something (models, FFmpeg). Retrying every file would burn the retry
    /// budget of the entire shoot in seconds and bury the UI in identical
    /// errors, so these requeue and wait for the situation to be fixed.
    Blocked(String),
    Failed(String),
}

/// How long a blocked worker waits before looking again. Long enough not to
/// spin, short enough that installing the models resumes work on its own.
const BLOCKED_BACKOFF: Duration = Duration::from_secs(5);

/// Blocked workers share one notice every this often, rather than one per
/// worker per file.
const BLOCKED_NOTICE_INTERVAL: Duration = Duration::from_secs(30);

fn should_announce_blockage() -> bool {
    static LAST: std::sync::OnceLock<parking_lot::Mutex<Option<Instant>>> = std::sync::OnceLock::new();
    let mut last = LAST.get_or_init(|| parking_lot::Mutex::new(None)).lock();
    match *last {
        Some(at) if at.elapsed() < BLOCKED_NOTICE_INTERVAL => false,
        _ => {
            *last = Some(Instant::now());
            true
        }
    }
}

fn run_job(
    app: &AppHandle,
    state: &Arc<AppState>,
    job: &Job,
    engine: &mut Option<Engine>,
    engine_version: &mut u64,
    ffmpeg: Option<&teo_media_core::Ffmpeg>,
) -> JobOutcome {
    let Some(kind) = JobKind::parse(&job.kind) else {
        return JobOutcome::Failed(format!("unknown job kind '{}'", job.kind));
    };
    let settings = state.settings();

    match kind {
        JobKind::Scan => {
            let cancel = state.cancellation(job.shoot_id);
            let app = app.clone();
            let shoot_id = job.shoot_id;
            match stages::scan_shoot(&state.db, shoot_id, &settings, Some(cancel), move |count| {
                events::emit(
                    &app,
                    events::NOTICE,
                    events::Notice {
                        level: "info".into(),
                        message: format!("Scanned {count} files…"),
                    },
                );
            }) {
                Ok(summary) => {
                    tracing::info!(
                        shoot = shoot_id,
                        photos = summary.photos,
                        videos = summary.videos,
                        "scan complete"
                    );
                    JobOutcome::Done
                }
                Err(e) => JobOutcome::Failed(e.to_string()),
            }
        }

        // Indexing runs without the engine on purpose — thumbnails and
        // metadata must work on a machine with no models installed.
        JobKind::Thumbnail => match load_media(state, job) {
            Ok(item) => {
                match crate::pipeline::index_media(&state.db, &state.thumbnails, ffmpeg, &item) {
                    Ok(()) => JobOutcome::Done,
                    Err(e) => JobOutcome::Failed(e.to_string()),
                }
            }
            Err(outcome) => outcome,
        },

        JobKind::AnalysePhoto | JobKind::AnalyseVideo => {
            run_media_job(state, job, engine, engine_version, |engine, db, item| {
                engine.analyse(db, item).map(|_| ())
            })
        }

        // The three shoot-wide stages must not start while per-file analysis is
        // still running, or they would work from a partial picture.
        JobKind::Recognise | JobKind::Cluster | JobKind::Albums => {
            if analysis_outstanding(state, job.shoot_id) {
                return JobOutcome::Deferred;
            }

            let result = match kind {
                JobKind::Recognise => stages::recognise_shoot(&state.db, job.shoot_id, &settings).map(|r| {
                    tracing::info!(
                        shoot = job.shoot_id,
                        matched = r.faces_matched,
                        examined = r.faces_examined,
                        "recognition complete"
                    );
                }),
                JobKind::Cluster => stages::cluster_shoot(&state.db, job.shoot_id, &settings).map(|r| {
                    tracing::info!(
                        shoot = job.shoot_id,
                        clusters = r.clusters_created,
                        faces = r.faces_clustered,
                        "clustering complete"
                    );
                }),
                _ => stages::generate_albums(&state.db, job.shoot_id).map(|created| {
                    tracing::info!(shoot = job.shoot_id, albums = created, "albums generated");
                }),
            };

            match result {
                Ok(()) => {
                    events::shoot_changed(app, job.shoot_id, kind.as_str());
                    JobOutcome::Done
                }
                Err(e) => JobOutcome::Failed(e.to_string()),
            }
        }
    }
}

/// Loads the media row a per-file job refers to.
fn load_media(state: &Arc<AppState>, job: &Job) -> std::result::Result<teo_database::models::Media, JobOutcome> {
    let Some(media_id) = job.media_id else {
        return Err(JobOutcome::Failed("job has no media id".into()));
    };
    match state.db.conn().and_then(|conn| media_repo::get_by_id(&conn, media_id)) {
        Ok(Some(item)) => Ok(item),
        Ok(None) => Err(JobOutcome::Failed(format!("media {media_id} no longer indexed"))),
        Err(e) => Err(JobOutcome::Failed(e.to_string())),
    }
}

/// Shared shape for the per-file jobs that *do* need AI: load the row, make
/// sure an engine exists, run the closure.
fn run_media_job(
    state: &Arc<AppState>,
    job: &Job,
    engine: &mut Option<Engine>,
    engine_version: &mut u64,
    action: impl FnOnce(&mut Engine, &teo_database::Database, &teo_database::models::Media) -> crate::pipeline::Result<()>,
) -> JobOutcome {
    let item = match load_media(state, job) {
        Ok(item) => item,
        Err(outcome) => return outcome,
    };

    // The I/O worker and AI worker deliberately run in parallel, but a newly
    // scanned media row starts with orientation=1 until its indexing job reads
    // EXIF/container metadata. Running AI during that window permanently puts
    // boxes and landmarks in the wrong coordinate system for rotated files.
    // Waiting on this one media row preserves lane parallelism while enforcing
    // the actual dependency.
    if indexing_incomplete(&item) {
        return JobOutcome::Deferred;
    }

    if engine.is_none() {
        let version = state.settings_version();
        match Engine::new(&state.paths, &state.settings()) {
            Ok(built) => {
                tracing::info!(detector = built.detector_name(), embedder = built.embedder_name(), "models loaded");
                *engine = Some(built);
                *engine_version = version;
            }
            // Systemic, not per-file: wait rather than consume this file's
            // retries and every other file's after it.
            Err(PipelineError::ModelsUnavailable(message)) => return JobOutcome::Blocked(message),
            Err(e) => return JobOutcome::Blocked(format!("could not load the face models: {e}")),
        }
    }

    let engine = engine.as_mut().expect("engine was just built");
    match action(engine, &state.db, &item) {
        Ok(()) => JobOutcome::Done,
        // A video on a machine with no FFmpeg is the same class of problem as
        // a missing model: no amount of retrying this file will help.
        Err(PipelineError::FfmpegUnavailable) => {
            JobOutcome::Blocked("FFmpeg is required for video analysis but was not found".into())
        }
        Err(e) => JobOutcome::Failed(e.to_string()),
    }
}

fn indexing_incomplete(item: &teo_database::models::Media) -> bool {
    item.processing_status == ProcessingStatus::Pending.as_str()
}

/// True while any per-file job for the shoot is still queued or running.
fn analysis_outstanding(state: &Arc<AppState>, shoot_id: i64) -> bool {
    let Ok(conn) = state.db.conn() else { return false };
    let outstanding: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs
              WHERE shoot_id = ?1 AND state IN ('queued','running')
                AND kind IN ('scan','thumbnail','analysePhoto','analyseVideo')",
            teo_database::rusqlite::params![shoot_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    outstanding > 0
}

fn finish_job(app: &AppHandle, state: &Arc<AppState>, job: &Job, outcome: JobOutcome) {
    let Ok(conn) = state.db.conn() else { return };

    match outcome {
        JobOutcome::Done => {
            let _ = jobs::complete(&conn, job.id);
        }
        JobOutcome::Deferred => {
            // Give the remaining analysis a moment rather than spinning on the
            // same row, and do not let waiting count against the retry budget.
            std::thread::sleep(IDLE_POLL);
            requeue_without_attempt(&conn, job.id);
        }
        JobOutcome::Blocked(reason) => {
            // The job goes back untouched. As soon as the missing piece is in
            // place, the next poll picks it up with no user action needed.
            tracing::warn!(job = job.id, kind = %job.kind, reason = %reason, "processing is blocked");
            if should_announce_blockage() {
                events::notice(app, "warn", format!("Processing paused: {reason}"));
            }
            std::thread::sleep(BLOCKED_BACKOFF);
            requeue_without_attempt(&conn, job.id);
        }
        JobOutcome::Failed(error) => {
            tracing::warn!(job = job.id, kind = %job.kind, error = %error, "job failed");

            let state_after = jobs::fail(&conn, job.id, &error).unwrap_or(JobState::Failed);
            if state_after == JobState::Failed {
                if let Some(media_id) = job.media_id {
                    let _ = media_repo::set_status(&conn, media_id, ProcessingStatus::Failed, Some(&error));
                }
                let file = job
                    .media_id
                    .and_then(|id| media_repo::get_by_id(&conn, id).ok().flatten())
                    .map(|m| m.filename);

                logs::record_quiet(
                    &conn,
                    logs::EVENT_PROCESSING_ERROR,
                    Some(job.shoot_id),
                    job.media_id,
                    None,
                    Some(&error),
                );
                events::emit(
                    app,
                    events::JOB_FAILED,
                    events::JobFailed {
                        shoot_id: job.shoot_id,
                        kind: job.kind.clone(),
                        file,
                        error,
                    },
                );
            }
        }
    }
}

/// Returns a job to the queue without charging it an attempt.
fn requeue_without_attempt(conn: &teo_database::rusqlite::Connection, job_id: i64) {
    let _ = conn.execute(
        "UPDATE jobs SET state = 'queued', started_at = NULL, attempts = MAX(attempts - 1, 0) WHERE id = ?1",
        teo_database::rusqlite::params![job_id],
    );
}

/// Pushes progress for every shoot that currently has work in the queue.
fn monitor_loop(app: AppHandle, state: Arc<AppState>) {
    let mut last_emit = Instant::now() - PROGRESS_INTERVAL;
    // Remembers which shoots were active last tick so a final "finished"
    // update is always delivered, even though the queue is empty by then.
    let mut previously_active: Vec<i64> = Vec::new();

    while !state.is_shutting_down() {
        std::thread::sleep(PROGRESS_INTERVAL);
        if last_emit.elapsed() < PROGRESS_INTERVAL {
            continue;
        }
        last_emit = Instant::now();

        let Ok(conn) = state.db.conn() else { continue };
        let active: Vec<i64> = conn
            .prepare("SELECT DISTINCT shoot_id FROM jobs WHERE state IN ('queued','running')")
            .and_then(|mut stmt| {
                let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap_or_default();

        let mut to_report = active.clone();
        for shoot_id in &previously_active {
            if !to_report.contains(shoot_id) {
                to_report.push(*shoot_id);
            }
        }

        for shoot_id in to_report {
            if let Ok(progress) = jobs::progress(&conn, shoot_id) {
                events::emit(
                    &app,
                    events::PROGRESS,
                    events::ProgressEvent { progress, paused: state.is_paused() },
                );
            }
        }

        previously_active = active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teo_database::models::MediaType;
    use teo_database::repo::shoots;
    use teo_database::Database;

    #[test]
    fn analysis_jobs_block_the_finishing_stages() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();

        let media_id = media_repo::upsert(
            &conn,
            &teo_database::models::NewMedia {
                shoot_id: shoot.id,
                path: "C:\\s\\a.jpg".into(),
                filename: "a.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "k".into(),
                captured_at: None,
            },
        )
        .unwrap();

        let analyse = jobs::enqueue(
            &conn,
            shoot.id,
            JobKind::AnalysePhoto,
            Some(media_id),
            stages::priority::ANALYSE_PHOTO,
            None,
        )
        .unwrap();
        jobs::enqueue(&conn, shoot.id, JobKind::Albums, None, stages::priority::ALBUMS, None).unwrap();

        let outstanding: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE shoot_id = ?1 AND state IN ('queued','running')
                   AND kind IN ('scan','thumbnail','analysePhoto','analyseVideo')",
                teo_database::rusqlite::params![shoot.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outstanding, 1, "the album stage must wait for this");

        jobs::claim_next(&conn, None).unwrap();
        jobs::complete(&conn, analyse).unwrap();

        let outstanding_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE shoot_id = ?1 AND state IN ('queued','running')
                   AND kind IN ('scan','thumbnail','analysePhoto','analyseVideo')",
                teo_database::rusqlite::params![shoot.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outstanding_after, 0);
    }

    #[test]
    fn deferring_does_not_consume_the_retry_budget() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();
        let id = jobs::enqueue(&conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();

        for _ in 0..10 {
            let job = jobs::claim_next(&conn, None).unwrap().unwrap();
            assert_eq!(job.id, id);
            conn.execute(
                "UPDATE jobs SET state = 'queued', started_at = NULL, attempts = MAX(attempts - 1, 0) WHERE id = ?1",
                teo_database::rusqlite::params![id],
            )
            .unwrap();
        }

        // Still runnable after ten deferrals — far more than MAX_ATTEMPTS.
        assert!(jobs::claim_next(&conn, None).unwrap().is_some());
    }

    #[test]
    fn analysis_waits_while_its_indexing_job_is_running() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();
        let media_id = media_repo::upsert(
            &conn,
            &teo_database::models::NewMedia {
                shoot_id: shoot.id,
                path: "C:\\s\\rotated.jpg".into(),
                filename: "rotated.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "rotated".into(),
                captured_at: None,
            },
        )
        .unwrap();
        jobs::enqueue(
            &conn,
            shoot.id,
            JobKind::Thumbnail,
            Some(media_id),
            stages::priority::INDEX,
            None,
        )
        .unwrap();
        jobs::enqueue(
            &conn,
            shoot.id,
            JobKind::AnalysePhoto,
            Some(media_id),
            stages::priority::ANALYSE_PHOTO,
            None,
        )
        .unwrap();

        // Recreate the production race: one lane has claimed indexing while
        // the compute lane has independently claimed analysis for the same
        // row. The row must still make analysis wait.
        let indexing = jobs::claim_next_io(&conn).unwrap().unwrap();
        let analysis = jobs::claim_next_compute(&conn).unwrap().unwrap();
        assert_eq!(indexing.kind, JobKind::Thumbnail.as_str());
        assert_eq!(analysis.kind, JobKind::AnalysePhoto.as_str());
        let before = media_repo::get_by_id(&conn, media_id).unwrap().unwrap();
        assert!(indexing_incomplete(&before));

        media_repo::set_status(&conn, media_id, ProcessingStatus::Thumbnailed, None).unwrap();
        let after = media_repo::get_by_id(&conn, media_id).unwrap().unwrap();
        assert!(!indexing_incomplete(&after));
    }
}
