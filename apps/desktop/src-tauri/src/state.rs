//! Shared application state.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use skwad_database::Database;
use skwad_media_core::{ThumbnailCache, VideoProxyCache};

use crate::catalogue::LoadedCatalogue;
use crate::paths::AppPaths;
use crate::settings::AppSettings;

pub struct AppState {
    pub db: Database,
    pub paths: AppPaths,
    pub thumbnails: ThumbnailCache,
    pub proxies: VideoProxyCache,
    /// Base URL the webview uses to fetch media through our custom protocol.
    pub media_url_base: String,

    settings: RwLock<AppSettings>,
    /// Bumped whenever settings change. Workers watch this and rebuild their
    /// inference sessions, so a threshold or accelerator change takes effect
    /// without restarting the application.
    settings_version: AtomicU64,

    /// Per-shoot cancellation flags, checked inside long-running stages.
    cancellations: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    /// Why a shoot's queue is stalled (missing FFmpeg, missing models), kept
    /// only in memory: a blockage is a fact about this run, not about the
    /// stored job. Entries age out on their own once work resumes.
    blockages: Mutex<HashMap<i64, Blockage>>,
    /// Global pause for the worker pool.
    paused: AtomicBool,
    scheduler: Mutex<Scheduler>,
    shutdown: Arc<AtomicBool>,
    pub loaded_catalogues: Mutex<HashMap<String, LoadedCatalogue>>,
}

#[derive(Default)]
struct Scheduler {
    paused_shoots: HashSet<i64>,
    last_compute_shoot: Option<i64>,
    last_io_shoot: Option<i64>,
}

impl AppState {
    /// A blockage is re-recorded every few seconds while it persists, so an
    /// entry older than this belongs to work that has since moved on.
    const BLOCKAGE_TTL: Duration = Duration::from_secs(20);

    pub fn new(db: Database, paths: AppPaths, settings: AppSettings, media_url_base: String) -> Self {
        let thumbnails = ThumbnailCache::new(&paths.thumbnails);
        let proxies = VideoProxyCache::new(&paths.proxies);
        Self {
            db,
            thumbnails,
            proxies,
            paths,
            media_url_base,
            settings: RwLock::new(settings),
            settings_version: AtomicU64::new(1),
            cancellations: Mutex::new(HashMap::new()),
            blockages: Mutex::new(HashMap::new()),
            paused: AtomicBool::new(false),
            scheduler: Mutex::new(Scheduler::default()),
            shutdown: Arc::new(AtomicBool::new(false)),
            loaded_catalogues: Mutex::new(HashMap::new()),
        }
    }

    pub fn settings(&self) -> AppSettings {
        self.settings.read().clone()
    }

    pub fn settings_version(&self) -> u64 {
        self.settings_version.load(Ordering::Acquire)
    }

    /// Replaces the settings and signals workers to reload.
    pub fn update_settings(&self, next: AppSettings) -> skwad_database::Result<AppSettings> {
        let next = next.sanitised();
        next.save(&self.db)?;
        *self.settings.write() = next.clone();
        self.settings_version.fetch_add(1, Ordering::Release);
        Ok(next)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_shoot_paused(&self, shoot_id: i64) -> bool {
        self.is_paused() || self.scheduler.lock().paused_shoots.contains(&shoot_id)
    }

    pub fn set_shoot_paused(&self, shoot_id: i64, paused: bool) {
        let mut scheduler = self.scheduler.lock();
        if paused {
            scheduler.paused_shoots.insert(shoot_id);
        } else {
            scheduler.paused_shoots.remove(&shoot_id);
        }
    }

    /// Serialize only claims, never decoding/inference. All compute workers
    /// share one cursor, so separate workers cannot each favour the first shoot.
    pub fn claim_job(
        &self,
        conn: &skwad_database::rusqlite::Connection,
        lane: skwad_database::repo::jobs::WorkerLane,
    ) -> skwad_database::Result<Option<skwad_database::models::Job>> {
        use skwad_database::repo::jobs::{self, WorkerLane};
        let mut scheduler = self.scheduler.lock();
        if self.is_paused() || self.is_shutting_down() {
            return Ok(None);
        }
        let mut excluded: Vec<i64> = scheduler.paused_shoots.iter().copied().collect();
        excluded.extend(
            self.cancellations
                .lock()
                .iter()
                .filter(|(_, flag)| flag.load(Ordering::Relaxed))
                .map(|(id, _)| *id),
        );
        let cursor = match lane {
            WorkerLane::Io => &mut scheduler.last_io_shoot,
            _ => &mut scheduler.last_compute_shoot,
        };
        let job = jobs::claim_next_parallel(conn, lane, *cursor, &excluded)?;
        if let Some(job) = &job {
            *cursor = Some(job.shoot_id);
        }
        Ok(job)
    }

    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    pub fn begin_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for flag in self.cancellations.lock().values() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Returns the cancellation flag for a shoot, creating it if needed.
    pub fn cancellation(&self, shoot_id: i64) -> Arc<AtomicBool> {
        Arc::clone(
            self.cancellations
                .lock()
                .entry(shoot_id)
                .or_insert_with(|| Arc::new(AtomicBool::new(false))),
        )
    }

    pub fn cancel_shoot(&self, shoot_id: i64) {
        self.cancellation(shoot_id).store(true, Ordering::Relaxed);
    }

    /// Clears a shoot's cancellation so processing can be started again.
    pub fn resume_shoot(&self, shoot_id: i64) {
        self.cancellation(shoot_id).store(false, Ordering::Relaxed);
        self.set_shoot_paused(shoot_id, false);
    }

    pub fn is_cancelled(&self, shoot_id: i64) -> bool {
        self.cancellations
            .lock()
            .get(&shoot_id)
            .is_some_and(|f| f.load(Ordering::Relaxed))
    }

    /// Records why a shoot's queue cannot move. Workers refresh this on every
    /// blocked attempt, so it stays current while the blockage lasts.
    pub fn record_blockage(&self, shoot_id: i64, kind: &str, reason: &str) {
        self.blockages.lock().insert(
            shoot_id,
            Blockage {
                kind: kind.to_string(),
                reason: reason.to_string(),
                at: Instant::now(),
            },
        );
    }

    /// The current blockage for a shoot, if a worker hit one recently.
    ///
    /// Nothing clears these explicitly: the moment work runs again the entry
    /// stops being refreshed and expires, which is also how a blockage fixed
    /// outside the app (installing FFmpeg) disappears from the UI on its own.
    pub fn blockage(&self, shoot_id: i64) -> Option<Blockage> {
        let mut blockages = self.blockages.lock();
        blockages.retain(|_, blockage| blockage.at.elapsed() < Self::BLOCKAGE_TTL);
        blockages.get(&shoot_id).cloned()
    }
}

/// A stalled queue, remembered long enough to explain itself in the UI.
#[derive(Debug, Clone)]
pub struct Blockage {
    /// The [`skwad_database::models::JobKind`] that could not run.
    pub kind: String,
    pub reason: String,
    at: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        let temp = std::env::temp_dir().join(format!("skwad-state-{}", std::process::id()));
        let paths = AppPaths::create(&temp).unwrap();
        AppState::new(
            Database::open_in_memory().unwrap(),
            paths,
            AppSettings::default(),
            "skwadmedia://localhost".into(),
        )
    }

    #[test]
    fn updating_settings_bumps_the_version() {
        let state = state();
        let before = state.settings_version();

        let mut next = state.settings();
        next.recognition_threshold = 0.66;
        state.update_settings(next).unwrap();

        assert!(state.settings_version() > before, "workers need a signal to reload");
        assert!((state.settings().recognition_threshold - 0.66).abs() < 1e-6);
    }

    #[test]
    fn settings_are_sanitised_on_the_way_in() {
        let state = state();
        let mut wild = state.settings();
        wild.recognition_threshold = 99.0;
        let stored = state.update_settings(wild).unwrap();
        assert!(stored.recognition_threshold <= 0.99);
    }

    #[test]
    fn cancellation_is_per_shoot_and_reversible() {
        let state = state();
        assert!(!state.is_cancelled(1));

        state.cancel_shoot(1);
        assert!(state.is_cancelled(1));
        assert!(!state.is_cancelled(2), "cancelling one shoot must not stop another");

        state.resume_shoot(1);
        assert!(!state.is_cancelled(1));
    }

    #[test]
    fn pausing_one_shoot_does_not_pause_other_shoots_or_consume_attempts() {
        use skwad_database::{
            models::JobKind,
            repo::{jobs, shoots},
        };
        let state = state();
        let conn = state.db.conn().unwrap();
        let a = shoots::create(&conn, "A", "A").unwrap();
        let b = shoots::create(&conn, "B", "B").unwrap();
        let a_job = jobs::enqueue(&conn, a.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        let b_job = jobs::enqueue(&conn, b.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
        state.set_shoot_paused(a.id, true);
        assert!(state.is_shoot_paused(a.id));
        assert!(!state.is_shoot_paused(b.id));
        assert!(!state.is_paused());
        assert_eq!(
            state.claim_job(&conn, jobs::WorkerLane::Compute).unwrap().unwrap().id,
            b_job
        );
        assert!(state.claim_job(&conn, jobs::WorkerLane::Compute).unwrap().is_none());
        assert_eq!(
            conn.query_row("SELECT attempts FROM jobs WHERE id=?1", [a_job], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        state.set_shoot_paused(a.id, false);
        assert_eq!(
            state.claim_job(&conn, jobs::WorkerLane::Compute).unwrap().unwrap().id,
            a_job
        );
    }

    #[test]
    fn parallel_worker_claims_are_distinct_and_shared_across_shoots() {
        use skwad_database::{
            models::JobKind,
            repo::{jobs, shoots},
        };
        let state = Arc::new(state());
        {
            let conn = state.db.conn().unwrap();
            for name in ["A", "B"] {
                let shoot = shoots::create(&conn, name, name).unwrap();
                for _ in 0..4 {
                    jobs::enqueue(&conn, shoot.id, JobKind::AnalyseVideo, None, 120, None).unwrap();
                }
            }
        }
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let conn = state.db.conn().unwrap();
                    state.claim_job(&conn, jobs::WorkerLane::Compute).unwrap().unwrap()
                })
            })
            .collect();
        let jobs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(jobs.iter().map(|j| j.id).collect::<HashSet<_>>().len(), 4);
        let mut counts = HashMap::new();
        for job in jobs {
            *counts.entry(job.shoot_id).or_insert(0) += 1;
        }
        assert_eq!(counts.values().copied().collect::<Vec<_>>(), vec![2, 2]);
    }

    #[test]
    fn shutdown_cancels_everything_in_flight() {
        let state = state();
        state.cancellation(1);
        state.cancellation(2);

        state.begin_shutdown();
        assert!(state.is_shutting_down());
        assert!(state.is_cancelled(1));
        assert!(state.is_cancelled(2));
    }
}
