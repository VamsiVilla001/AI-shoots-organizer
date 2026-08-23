//! Shared application state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use teo_database::Database;
use teo_media_core::ThumbnailCache;

use crate::paths::AppPaths;
use crate::progress::ProgressSink;
use crate::settings::AppSettings;

pub struct AppState {
    pub db: Database,
    pub paths: AppPaths,
    pub thumbnails: ThumbnailCache,
    /// Base URL a front end uses to fetch media: the custom protocol in the
    /// desktop app, an HTTP path prefix when served over the network.
    pub media_url_base: String,

    /// Where pushed events go. The core never knows what is on the other end.
    sink: Arc<dyn ProgressSink>,

    settings: RwLock<AppSettings>,
    /// Bumped whenever settings change. Workers watch this and rebuild their
    /// inference sessions, so a threshold or accelerator change takes effect
    /// without restarting the application.
    settings_version: AtomicU64,

    /// Per-shoot cancellation flags, checked inside long-running stages.
    cancellations: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    /// Global pause for the worker pool.
    paused: AtomicBool,
    shutdown: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(
        db: Database,
        paths: AppPaths,
        settings: AppSettings,
        media_url_base: String,
        sink: Arc<dyn ProgressSink>,
    ) -> Self {
        let thumbnails = ThumbnailCache::new(&paths.thumbnails);
        Self {
            db,
            thumbnails,
            paths,
            media_url_base,
            sink,
            settings: RwLock::new(settings),
            settings_version: AtomicU64::new(1),
            cancellations: Mutex::new(HashMap::new()),
            paused: AtomicBool::new(false),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The event destination for anything running against this state.
    pub fn sink(&self) -> &dyn ProgressSink {
        self.sink.as_ref()
    }

    /// A cloneable handle, for work that outlives the borrow — a spawned
    /// export thread, say.
    pub fn sink_handle(&self) -> Arc<dyn ProgressSink> {
        Arc::clone(&self.sink)
    }

    pub fn settings(&self) -> AppSettings {
        self.settings.read().clone()
    }

    pub fn settings_version(&self) -> u64 {
        self.settings_version.load(Ordering::Acquire)
    }

    /// Replaces the settings and signals workers to reload.
    pub fn update_settings(&self, next: AppSettings) -> teo_database::Result<AppSettings> {
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
    }

    pub fn is_cancelled(&self, shoot_id: i64) -> bool {
        self.cancellations
            .lock()
            .get(&shoot_id)
            .is_some_and(|f| f.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        let temp = std::env::temp_dir().join(format!("teo-state-{}", std::process::id()));
        let paths = AppPaths::create(&temp).unwrap();
        AppState::new(
            Database::open_in_memory().unwrap(),
            paths,
            AppSettings::default(),
            "teomedia://localhost".into(),
            crate::progress::null_sink(),
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
