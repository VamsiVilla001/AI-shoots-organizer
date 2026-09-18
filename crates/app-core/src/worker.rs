//! The background worker pool (§18).
//!
//! Workers pull jobs from the queue in PostgreSQL, so the queue survives a
//! crash or a quit: every claim is a lease, a lease keeper thread heartbeats
//! the jobs this process holds, and anything whose lease lapses — because the
//! process died, or the machine went to sleep — is returned to the queue by
//! the reaper and picked up again. Nothing here blocks the UI thread.

use skwad_database::Db;
use std::sync::Arc;
use std::time::{Duration, Instant};

use skwad_database::models::{Job, JobKind, JobState, ProcessingStatus};
use skwad_database::repo::{jobs, logs, media as media_repo, telemetry};
use crate::events;
use crate::job_source::{JobSource, LocalJobSource, Settled};
use crate::progress::ProgressSink;
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

/// A two-second sample catches short GPU bursts without materially affecting
/// processing or making long-run telemetry large.
const RESOURCE_INTERVAL: Duration = Duration::from_secs(2);

/// How often the monitor returns lapsed leases to the queue. Coarser than the
/// heartbeat so a healthy worker always beats it, finer than the TTL so a dead
/// one's jobs are back within a lease of dying.
const REAP_INTERVAL: Duration = Duration::from_secs(15);

pub struct WorkerPool {
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl WorkerPool {
    /// One I/O worker and bounded AI slots. Disabled slots stay idle without
    /// loading models, allowing concurrency changes without restarting the app.
    pub fn start(sink: Arc<dyn ProgressSink>, state: Arc<AppState>) -> Self {
        // Recover anything a previous run *of this machine* left mid-flight.
        // Other machines' running jobs are theirs until their lease lapses.
        match state
            .db
            .conn()
            .and_then(|mut conn| jobs::requeue_stale(&mut conn, &state.machine_id))
        {
            Ok(n) if n > 0 => tracing::info!(jobs = n, "recovered interrupted jobs from the previous session"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "could not recover interrupted jobs"),
        }
        let local = LocalJobSource::new(Arc::clone(&state));
        let leases = Arc::clone(local.leases());
        let source: Arc<dyn JobSource> = Arc::new(local);
        let worker_count = crate::settings::MAX_AI_WORKERS + 1;
        let mut handles = Vec::with_capacity(worker_count + 2);

        for index in 0..worker_count {
            let app = Arc::clone(&sink);
            let state = Arc::clone(&state);
            let source = Arc::clone(&source);
            handles.push(
                std::thread::Builder::new()
                    .name(format!("skwad-worker-{index}"))
                    .spawn(move || worker_loop(index, app, state, source))
                    .expect("failed to spawn worker thread"),
            );
        }

        let keeper_state = Arc::clone(&state);
        let keeper_leases = Arc::clone(&leases);
        handles.push(
            std::thread::Builder::new()
                .name("skwad-lease-keeper".into())
                .spawn(move || LocalJobSource::keeper_loop(keeper_state, keeper_leases))
                .expect("failed to spawn the lease keeper thread"),
        );

        let monitor_app = Arc::clone(&sink);
        let monitor_state = Arc::clone(&state);
        handles.push(
            std::thread::Builder::new()
                .name("skwad-monitor".into())
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

fn worker_loop(index: usize, app: Arc<dyn ProgressSink>, state: Arc<AppState>, source: Arc<dyn JobSource>) {
    tracing::debug!(worker = index, "worker started");

    // Built on first use: a session that only ever browses an existing shoot
    // should not pay to load two ONNX models.
    let mut engine: Option<Engine> = None;
    let mut engine_last_used: Option<Instant> = None;
    let mut engine_version: u64 = 0;
    // FFmpeg is resolved per worker so thumbnail jobs never need the engine —
    // indexing works with no models installed.
    let mut tools_version = source.settings_version();
    let mut ffmpeg = crate::pipeline::discover_ffmpeg(&source.settings());
    let lane = if index == 0 {
        jobs::WorkerLane::Io
    } else {
        jobs::WorkerLane::Compute
    };

    while !state.is_shutting_down() {
        if index > source.settings().ai_workers.clamp(1, crate::settings::MAX_AI_WORKERS) {
            engine = None;
            engine_last_used = None;
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        if source.is_paused() {
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        // Each AI worker owns its model pair. A shared scheduler distributes
        // distinct media fairly and holds finishing stages behind all analyses.
        let claimed = match source.claim(lane) {
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
        source.hold(&job);

        // A pause can arrive just after an atomic claim. Give the job back
        // without consuming an attempt; already executing files finish safely.
        if source.is_shoot_paused(job.shoot_id) {
            source.release(&job);
            continue;
        }

        // A cancelled shoot's remaining jobs are dropped rather than run.
        if source.is_cancelled(job.shoot_id) {
            source.cancel_shoot_jobs(job.shoot_id);
            source.drop_lease(job.id);
            continue;
        }

        // Settings changed since these were built: rebuild so new thresholds,
        // accelerator choices and the FFmpeg path apply immediately.
        if tools_version != source.settings_version() {
            if engine.is_some() {
                tracing::info!(worker = index, "settings changed; reloading models");
                engine = None;
                engine_last_used = None;
            }
            ffmpeg = crate::pipeline::discover_ffmpeg(&source.settings());
            tools_version = source.settings_version();
        }

        if let Ok(mut conn) = state.db.conn() {
            if let Err(error) = telemetry::mark_stage_started(&mut conn, job.shoot_id, &job.kind) {
                tracing::warn!(shoot = job.shoot_id, error = %error, "could not start processing telemetry");
            }
        }

        let outcome = run_job(&app, &state, source.as_ref(), &job, &mut engine, &mut engine_version, ffmpeg.as_ref());
        if matches!(
            JobKind::parse(&job.kind),
            Some(JobKind::AnalysePhoto | JobKind::AnalyseVideo)
        ) {
            engine_last_used = Some(Instant::now());
        }
        finish_job(&app, &state, source.as_ref(), &job, outcome);
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
    app: &Arc<dyn ProgressSink>,
    state: &Arc<AppState>,
    source: &dyn JobSource,
    job: &Job,
    engine: &mut Option<Engine>,
    engine_version: &mut u64,
    ffmpeg: Option<&skwad_media_core::Ffmpeg>,
) -> JobOutcome {
    let Some(kind) = JobKind::parse(&job.kind) else {
        return JobOutcome::Failed(format!("unknown job kind '{}'", job.kind));
    };
    let settings = source.settings();

    match kind {
        JobKind::Scan => {
            let cancel = source.cancel_flag(job.shoot_id);
            let app = Arc::clone(app);
            let shoot_id = job.shoot_id;
            match stages::scan_shoot(&state.db, shoot_id, &settings, Some(cancel), move |count| {
                events::emit(
                    app.as_ref(),
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
            Ok(item) => match crate::pipeline::index_media(&state.db, &state.thumbnails, ffmpeg, &item) {
                Ok(()) => JobOutcome::Done,
                Err(e) => JobOutcome::Failed(e.to_string()),
            },
            Err(outcome) => outcome,
        },

        // Proxy generation is currently disabled. Complete legacy queued jobs
        // without touching the source media or starting a transcoder.
        JobKind::Proxy => JobOutcome::Done,

        JobKind::AnalysePhoto | JobKind::AnalyseVideo => {
            run_media_job(state, source, job, engine, engine_version, |engine, db, item| {
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
                    events::shoot_changed(app.as_ref(), job.shoot_id, kind.as_str());
                    JobOutcome::Done
                }
                Err(e) => JobOutcome::Failed(e.to_string()),
            }
        }
    }
}

/// Loads the media row a per-file job refers to.
fn load_media(state: &Arc<AppState>, job: &Job) -> std::result::Result<skwad_database::models::Media, JobOutcome> {
    let Some(media_id) = job.media_id else {
        return Err(JobOutcome::Failed("job has no media id".into()));
    };
    match state
        .db
        .conn()
        .and_then(|mut conn| media_repo::get_by_id(&mut conn, media_id))
    {
        Ok(Some(item)) => Ok(item),
        Ok(None) => Err(JobOutcome::Failed(format!("media {media_id} no longer indexed"))),
        Err(e) => Err(JobOutcome::Failed(e.to_string())),
    }
}

/// Shared shape for the per-file jobs that *do* need AI: load the row, make
/// sure an engine exists, run the closure.
fn run_media_job(
    state: &Arc<AppState>,
    source: &dyn JobSource,
    job: &Job,
    engine: &mut Option<Engine>,
    engine_version: &mut u64,
    action: impl FnOnce(
        &mut Engine,
        &skwad_database::Database,
        &skwad_database::models::Media,
    ) -> crate::pipeline::Result<()>,
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
        let version = source.settings_version();
        match Engine::new(&state.paths, &source.settings()) {
            Ok(built) => {
                tracing::info!(
                    detector = built.detector_name(),
                    embedder = built.embedder_name(),
                    "models loaded"
                );
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

fn indexing_incomplete(item: &skwad_database::models::Media) -> bool {
    item.processing_status == ProcessingStatus::Pending.as_str()
}

/// True while any per-file job for the shoot is still queued or running.
fn analysis_outstanding(state: &Arc<AppState>, shoot_id: i64) -> bool {
    let Ok(mut conn) = state.db.conn() else { return false };
    let outstanding: i64 = conn
        .row_one(
            "SELECT COUNT(*) FROM jobs
              WHERE shoot_id = $1 AND state IN ('queued','running')
                AND kind IN ('scan','thumbnail','analysePhoto','analyseVideo')",
            skwad_database::params![shoot_id],
        )
        .map(|row| row.get(0))
        .unwrap_or(0);
    outstanding > 0
}

fn finish_job(app: &Arc<dyn ProgressSink>, state: &Arc<AppState>, source: &dyn JobSource, job: &Job, outcome: JobOutcome) {
    let Ok(mut conn) = state.db.conn() else {
        source.drop_lease(job.id);
        return;
    };

    let mut settled = false;
    let mut succeeded = false;

    match outcome {
        JobOutcome::Done => {
            match source.complete(job) {
                Settled::Done => {
                    settled = true;
                    succeeded = true;
                }
                // The lease lapsed (or the shoot was cancelled) while the
                // stage ran. Whatever it wrote is either already superseded
                // by a newer holder's run or belongs to a cancelled shoot;
                // either way this job is not ours to settle.
                Settled::LeaseLost => tracing::warn!(job = job.id, kind = %job.kind, "job finished after its lease was lost; result not recorded"),
            }
        }
        JobOutcome::Deferred => {
            // Give the remaining analysis a moment rather than spinning on the
            // same row, and do not let waiting count against the retry budget.
            std::thread::sleep(IDLE_POLL);
            drop(conn);
            source.release(job);
            return;
        }
        JobOutcome::Blocked(reason) => {
            // The job goes back untouched. As soon as the missing piece is in
            // place, the next poll picks it up with no user action needed.
            tracing::warn!(job = job.id, kind = %job.kind, reason = %reason, "processing is blocked");
            // The progress panel reads this so a stalled queue explains itself
            // instead of looking like slow work.
            source.record_blockage(job.shoot_id, &job.kind, &reason);
            if should_announce_blockage() {
                events::notice(app.as_ref(), "warn", format!("Processing paused: {reason}"));
            }
            std::thread::sleep(BLOCKED_BACKOFF);
            drop(conn);
            source.release(job);
            return;
        }
        JobOutcome::Failed(error) => {
            tracing::warn!(job = job.id, kind = %job.kind, error = %error, "job failed");

            let state_after = source.fail(job, &error);
            if state_after.is_none() {
                tracing::warn!(job = job.id, "job failed after its lease was lost; failure not recorded");
            }
            if state_after == Some(JobState::Failed) {
                settled = true;
                if let Some(media_id) = job.media_id {
                    let _ = media_repo::set_status(&mut conn, media_id, ProcessingStatus::Failed, Some(&error));
                }
                let file = job
                    .media_id
                    .and_then(|id| media_repo::get_by_id(&mut conn, id).ok().flatten())
                    .map(|m| m.filename);

                logs::record_quiet(
                    &mut conn,
                    logs::EVENT_PROCESSING_ERROR,
                    Some(job.shoot_id),
                    job.media_id,
                    None,
                    Some(&error),
                );
                events::emit(
                    app.as_ref(),
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
    source.drop_lease(job.id);

    if settled {
        if let Err(error) = telemetry::mark_stage_settled(&mut conn, job.shoot_id, &job.kind, succeeded) {
            tracing::warn!(shoot = job.shoot_id, error = %error, "could not finish stage telemetry");
        }
        if let Err(error) = telemetry::finalize_if_settled(&mut conn, job.shoot_id) {
            tracing::warn!(shoot = job.shoot_id, error = %error, "could not finish processing telemetry");
        }
    }
}

/// Pushes progress for every shoot that currently has work in the queue, and
/// runs the lease reaper.
fn monitor_loop(app: Arc<dyn ProgressSink>, state: Arc<AppState>) {
    let mut last_emit = Instant::now() - PROGRESS_INTERVAL;
    let mut last_resource = Instant::now() - RESOURCE_INTERVAL;
    let mut last_reap = Instant::now();
    let mut resource_monitor = crate::resource_monitor::ResourceMonitor::new();
    // Remembers which shoots were active last tick so a final "finished"
    // update is always delivered, even though the queue is empty by then.
    let mut previously_active: Vec<i64> = Vec::new();

    while !state.is_shutting_down() {
        std::thread::sleep(PROGRESS_INTERVAL);
        if last_emit.elapsed() < PROGRESS_INTERVAL {
            continue;
        }
        last_emit = Instant::now();

        let Ok(mut conn) = state.db.conn() else { continue };

        // Leases whose holder stopped heartbeating — a crashed worker, a
        // laptop shut mid-job — go back to the queue here. Any process
        // brokering the queue may reap, because the reaper only ever touches
        // rows whose expiry has passed.
        if last_reap.elapsed() >= REAP_INTERVAL {
            last_reap = Instant::now();
            match jobs::reap_expired(&mut conn) {
                Ok(reaped) if reaped.requeued > 0 || reaped.failed > 0 => {
                    tracing::info!(requeued = reaped.requeued, failed = reaped.failed, "reaped lapsed job leases");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "could not reap lapsed job leases"),
            }
        }

        let active: Vec<i64> = conn
            .rows(
                "SELECT DISTINCT shoot_id FROM jobs WHERE state IN ('queued','running')",
                skwad_database::params![],
            )
            .map(|rows| rows.iter().map(|r| r.get::<_, i64>(0)).collect())
            .unwrap_or_default();

        let mut to_report = active.clone();
        for shoot_id in &previously_active {
            if !to_report.contains(shoot_id) {
                to_report.push(*shoot_id);
            }
        }

        // Start the CPU baseline when processing wakes up. Otherwise the first
        // point after a long idle period would average that idle time into the
        // shoot and under-report its actual load.
        if previously_active.is_empty() && !active.is_empty() {
            resource_monitor = crate::resource_monitor::ResourceMonitor::new();
            last_resource = Instant::now();
        }

        let just_finished: Vec<i64> = previously_active
            .iter()
            .copied()
            .filter(|shoot_id| !active.contains(shoot_id))
            .collect();
        if (!active.is_empty() && last_resource.elapsed() >= RESOURCE_INTERVAL) || !just_finished.is_empty() {
            let usage = resource_monitor.sample();
            let concurrent = active.len().max(1) as i64;
            for shoot_id in &active {
                let workers: i64 = conn
                    .row_one(
                        "SELECT COUNT(*) FROM jobs WHERE shoot_id = $1 AND state = 'running'",
                        skwad_database::params![shoot_id],
                    )
                    .map(|row| row.get(0))
                    .unwrap_or(0);
                if let Err(error) = telemetry::record_sample(
                    &mut conn,
                    *shoot_id,
                    usage.cpu_percent,
                    usage.gpu_percent,
                    workers,
                    concurrent,
                    false,
                ) {
                    tracing::warn!(shoot = shoot_id, error = %error, "could not save resource sample");
                }
            }
            for shoot_id in &just_finished {
                if let Err(error) = telemetry::record_sample(
                    &mut conn,
                    *shoot_id,
                    usage.cpu_percent,
                    usage.gpu_percent,
                    0,
                    concurrent,
                    true,
                ) {
                    tracing::warn!(shoot = shoot_id, error = %error, "could not save final resource sample");
                }
            }
            last_resource = Instant::now();
        }

        for shoot_id in to_report {
            if let Ok(mut progress) = jobs::progress(&mut conn, shoot_id) {
                if let Some(blockage) = state.blockage(shoot_id) {
                    progress.blocked_kind = Some(blockage.kind);
                    progress.blocked_reason = Some(blockage.reason);
                }
                events::emit(
                    app.as_ref(),
                    events::PROGRESS,
                    events::ProgressEvent {
                        progress,
                        paused: state.is_shoot_paused(shoot_id),
                    },
                );
            }
        }

        previously_active = active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skwad_database::models::MediaType;
    use skwad_database::repo::shoots;
    use skwad_database::Database;

    #[test]
    fn analysis_jobs_block_the_finishing_stages() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();

        let media_id = media_repo::upsert(
            &mut conn,
            &skwad_database::models::NewMedia {
                shoot_id: shoot.id,
                path: "C:\\s\\a.jpg".into(),
                filename: "a.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "k".into(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )
        .unwrap();

        let analyse = jobs::enqueue(
            &mut conn,
            shoot.id,
            JobKind::AnalysePhoto,
            Some(media_id),
            stages::priority::ANALYSE_PHOTO,
            None,
        )
        .unwrap();
        jobs::enqueue(
            &mut conn,
            shoot.id,
            JobKind::Albums,
            None,
            stages::priority::ALBUMS,
            None,
        )
        .unwrap();

        let outstanding: i64 = conn
            .row_one(
                "SELECT COUNT(*) FROM jobs WHERE shoot_id = $1 AND state IN ('queued','running')
                   AND kind IN ('scan','thumbnail','analysePhoto','analyseVideo')",
                skwad_database::params![shoot.id],
            )
            .unwrap()
            .get(0);
        assert_eq!(outstanding, 1, "the album stage must wait for this");

        let claimed = jobs::claim_next(&mut conn, None, "test").unwrap().unwrap();
        assert_eq!(claimed.id, analyse);
        assert!(jobs::complete(&mut conn, analyse, claimed.token().unwrap()).unwrap());

        let outstanding_after: i64 = conn
            .row_one(
                "SELECT COUNT(*) FROM jobs WHERE shoot_id = $1 AND state IN ('queued','running')
                   AND kind IN ('scan','thumbnail','analysePhoto','analyseVideo')",
                skwad_database::params![shoot.id],
            )
            .unwrap()
            .get(0);
        assert_eq!(outstanding_after, 0);
    }

    #[test]
    fn deferring_does_not_consume_the_retry_budget() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        let id = jobs::enqueue(&mut conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();

        for _ in 0..10 {
            let job = jobs::claim_next(&mut conn, None, "test").unwrap().unwrap();
            assert_eq!(job.id, id);
            assert!(jobs::release(&mut conn, id, job.token().unwrap()).unwrap());
        }

        // Still runnable after ten deferrals — far more than MAX_ATTEMPTS.
        assert!(jobs::claim_next(&mut conn, None, "test").unwrap().is_some());
    }

    #[test]
    fn analysis_waits_while_its_indexing_job_is_running() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        let media_id = media_repo::upsert(
            &mut conn,
            &skwad_database::models::NewMedia {
                shoot_id: shoot.id,
                path: "C:\\s\\rotated.jpg".into(),
                filename: "rotated.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "rotated".into(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )
        .unwrap();
        jobs::enqueue(
            &mut conn,
            shoot.id,
            JobKind::Thumbnail,
            Some(media_id),
            stages::priority::INDEX,
            None,
        )
        .unwrap();
        jobs::enqueue(
            &mut conn,
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
        let indexing = jobs::claim_next_io(&mut conn, "test").unwrap().unwrap();
        let analysis = jobs::claim_next_compute(&mut conn, "test").unwrap().unwrap();
        assert_eq!(indexing.kind, JobKind::Thumbnail.as_str());
        assert_eq!(analysis.kind, JobKind::AnalysePhoto.as_str());
        let before = media_repo::get_by_id(&mut conn, media_id).unwrap().unwrap();
        assert!(indexing_incomplete(&before));

        media_repo::set_status(&mut conn, media_id, ProcessingStatus::Thumbnailed, None).unwrap();
        let after = media_repo::get_by_id(&mut conn, media_id).unwrap().unwrap();
        assert!(!indexing_incomplete(&after));
    }

}
