//! User-facing settings, persisted in the `settings` table.
//!
//! Every threshold the recognition pipeline uses is here rather than baked into
//! the algorithms — §15 requires recognition thresholds to stay configurable,
//! and in practice a shoot with heavy stage lighting needs different numbers
//! from a clean studio session.

use serde::{Deserialize, Serialize};
use teo_clustering::{ClusterConfig, MatcherConfig};
use teo_database::{repo::settings, Database, Result as DbResult};
use teo_face_detection::{Accelerator, DetectorConfig, SessionConfig};
use teo_video_analysis::VideoAnalysisConfig;

const KEY: &str = "app_settings";
/// More than two concurrent decoders and ONNX engine pairs makes the desktop
/// shell compete with its own background work for RAM, CPU and GPU time.
const MAX_BACKGROUND_WORKERS: usize = 2;
/// Leave CPU capacity for the webview, SQLite and image decoding even on large
/// workstations. ONNX inference scales poorly beyond this per-session limit.
const MAX_INFERENCE_THREADS: usize = 4;
const LEGACY_RECOGNITION_THRESHOLD: f32 = 0.42;
const LEGACY_RECOGNITION_MARGIN: f32 = 0.05;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    // --- AI runtime -------------------------------------------------------
    pub accelerator: Accelerator,
    /// Threads per inference session.
    pub inference_threads: usize,
    /// How many files are analysed at once. AI sessions are memory-hungry, so
    /// this is deliberately conservative.
    pub worker_threads: usize,

    // --- Detection --------------------------------------------------------
    pub detection_threshold: f32,
    pub detection_nms_threshold: f32,
    pub detection_input_size: u32,
    pub max_faces_per_image: usize,
    /// Longest edge an image is resized to before detection. Running AI on a
    /// resized copy is the single biggest performance lever (§19).
    pub analysis_max_dim: u32,

    // --- Recognition ------------------------------------------------------
    pub recognition_threshold: f32,
    pub recognition_margin: f32,
    /// Prevents one photo being labelled with the same player twice.
    pub unique_person_per_frame: bool,
    /// Automatically confirm matches above this score instead of leaving them
    /// for review. 1.0 disables it — nothing is ever auto-confirmed.
    pub auto_confirm_above: f32,

    // --- Clustering -------------------------------------------------------
    pub cluster_edge_threshold: f32,
    pub cluster_min_size: usize,
    pub cluster_merge_threshold: f32,
    pub cluster_neighbours: usize,

    // --- Video ------------------------------------------------------------
    pub video_enabled: bool,
    pub video_scene_threshold: f64,
    pub video_sample_interval: f64,
    pub video_max_frames: usize,

    // --- Scanning ---------------------------------------------------------
    pub scan_recursive: bool,
    /// Explicit FFmpeg location, for installs that are not on `PATH`.
    pub ffmpeg_directory: Option<String>,

    // --- Models -----------------------------------------------------------
    pub detector_model: Option<String>,
    pub embedder_model: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        let cores = num_cpus::get();
        let worker_threads = cores.clamp(1, MAX_BACKGROUND_WORKERS);
        let background_cores = cores.saturating_sub(1).max(1);
        Self {
            accelerator: Accelerator::Auto,
            // Threads are shared out across the workers rather than handed to
            // each in full. The previous default gave every worker `cores / 2`,
            // so on a 16-core machine four workers asked for 32 threads and
            // spent much of their time fighting each other for cores.
            inference_threads: (background_cores / worker_threads).clamp(1, MAX_INFERENCE_THREADS),
            worker_threads,

            detection_threshold: 0.5,
            detection_nms_threshold: 0.4,
            detection_input_size: 640,
            max_faces_per_image: 64,
            analysis_max_dim: 1600,

            recognition_threshold: 0.55,
            recognition_margin: 0.10,
            unique_person_per_frame: true,
            auto_confirm_above: 1.0,

            cluster_edge_threshold: 0.45,
            cluster_min_size: 3,
            cluster_merge_threshold: 0.62,
            cluster_neighbours: 12,

            video_enabled: true,
            video_scene_threshold: 0.3,
            video_sample_interval: 5.0,
            video_max_frames: 60,

            scan_recursive: true,
            ffmpeg_directory: None,

            detector_model: None,
            embedder_model: None,
        }
    }
}

impl AppSettings {
    pub fn load(db: &Database) -> DbResult<Self> {
        let conn = db.conn()?;
        let mut loaded: Self = settings::get(&conn, KEY, AppSettings::default())?;
        // Upgrade installations that still carry the original permissive
        // pair. Custom values are respected; only the exact legacy defaults
        // are migrated.
        if (loaded.recognition_threshold - LEGACY_RECOGNITION_THRESHOLD).abs() < f32::EPSILON
            && (loaded.recognition_margin - LEGACY_RECOGNITION_MARGIN).abs() < f32::EPSILON
        {
            loaded.recognition_threshold = AppSettings::default().recognition_threshold;
            loaded.recognition_margin = AppSettings::default().recognition_margin;
            settings::set(&conn, KEY, &loaded)?;
        }
        Ok(loaded)
    }

    pub fn save(&self, db: &Database) -> DbResult<()> {
        let conn = db.conn()?;
        settings::set(&conn, KEY, self)
    }

    /// Clamps every value into a range the pipeline can actually work with.
    /// Settings arrive from the UI and from a JSON blob written by an older
    /// build, so neither source is trusted.
    pub fn sanitised(mut self) -> Self {
        let cores = num_cpus::get();
        self.worker_threads = self.worker_threads.clamp(1, cores.clamp(1, MAX_BACKGROUND_WORKERS));
        let background_cores = cores.saturating_sub(1).max(1);
        let max_threads = (background_cores / self.worker_threads).clamp(1, MAX_INFERENCE_THREADS);
        self.inference_threads = self.inference_threads.clamp(1, max_threads);

        self.detection_threshold = self.detection_threshold.clamp(0.05, 0.99);
        self.detection_nms_threshold = self.detection_nms_threshold.clamp(0.1, 0.9);
        // The detector's strides are 8/16/32, so the input must be a multiple
        // of 32 or the feature-map arithmetic does not line up.
        self.detection_input_size = (self.detection_input_size.clamp(320, 1280) / 32) * 32;
        self.max_faces_per_image = self.max_faces_per_image.clamp(1, 256);
        self.analysis_max_dim = self.analysis_max_dim.clamp(640, 4096);

        self.recognition_threshold = self.recognition_threshold.clamp(0.1, 0.99);
        self.recognition_margin = self.recognition_margin.clamp(0.0, 0.5);
        self.auto_confirm_above = self.auto_confirm_above.clamp(0.0, 1.0);

        self.cluster_edge_threshold = self.cluster_edge_threshold.clamp(0.1, 0.99);
        self.cluster_min_size = self.cluster_min_size.clamp(1, 100);
        self.cluster_merge_threshold = self.cluster_merge_threshold.clamp(0.1, 1.0);
        self.cluster_neighbours = self.cluster_neighbours.clamp(2, 64);

        self.video_scene_threshold = self.video_scene_threshold.clamp(0.05, 0.95);
        self.video_sample_interval = self.video_sample_interval.clamp(0.0, 600.0);
        self.video_max_frames = self.video_max_frames.clamp(1, 1000);

        self
    }

    pub fn session_config(&self) -> SessionConfig {
        SessionConfig {
            accelerator: self.accelerator,
            intra_threads: self.inference_threads,
        }
    }

    pub fn detector_config(&self) -> DetectorConfig {
        DetectorConfig {
            score_threshold: self.detection_threshold,
            nms_threshold: self.detection_nms_threshold,
            input_size: self.detection_input_size,
            max_faces: self.max_faces_per_image,
        }
    }

    pub fn matcher_config(&self) -> MatcherConfig {
        MatcherConfig {
            threshold: self.recognition_threshold,
            margin: self.recognition_margin,
            unique_per_frame: self.unique_person_per_frame,
        }
    }

    pub fn cluster_config(&self) -> ClusterConfig {
        ClusterConfig {
            edge_threshold: self.cluster_edge_threshold,
            neighbours: self.cluster_neighbours,
            iterations: 24,
            min_cluster_size: self.cluster_min_size,
            merge_threshold: self.cluster_merge_threshold,
        }
    }

    pub fn video_config(&self) -> VideoAnalysisConfig {
        VideoAnalysisConfig {
            scene_threshold: self.video_scene_threshold,
            sample_interval: self.video_sample_interval,
            max_frames: self.video_max_frames,
            probe_fps: 4.0,
            // Detection ultimately runs on a 640px tensor. A 1280px video
            // frame retains ample crop detail for recognition while avoiding
            // the extra scaling and RGB memory of a 1600px intermediate.
            frame_max_dim: self.analysis_max_dim.min(1280),
            min_frame_gap: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_sanitising_unchanged() {
        let defaults = AppSettings::default();
        let sanitised = defaults.clone().sanitised();
        assert_eq!(sanitised.detection_threshold, defaults.detection_threshold);
        assert_eq!(sanitised.detection_input_size, defaults.detection_input_size);
        assert_eq!(sanitised.recognition_threshold, defaults.recognition_threshold);
    }

    /// Workers each build their own inference session, so handing every one of
    /// them a full share of the machine oversubscribes it badly.
    #[test]
    fn default_threads_do_not_oversubscribe_the_machine() {
        let settings = AppSettings::default();
        let cores = num_cpus::get();
        assert!(
            settings.worker_threads * settings.inference_threads <= cores.max(1),
            "{} workers x {} threads exceeds {cores} cores",
            settings.worker_threads,
            settings.inference_threads
        );
        assert!(settings.inference_threads >= 1);
        assert!(settings.worker_threads >= 1);
        assert!(settings.worker_threads <= MAX_BACKGROUND_WORKERS);
        assert!(settings.inference_threads <= MAX_INFERENCE_THREADS);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let wild = AppSettings {
            detection_threshold: 5.0,
            recognition_threshold: -1.0,
            cluster_min_size: 0,
            worker_threads: 9999,
            analysis_max_dim: 10,
            video_max_frames: 0,
            ..Default::default()
        }
        .sanitised();

        assert!(wild.detection_threshold <= 0.99);
        assert!(wild.recognition_threshold >= 0.1);
        assert_eq!(wild.cluster_min_size, 1);
        assert!(wild.worker_threads <= MAX_BACKGROUND_WORKERS);
        assert!(wild.inference_threads <= MAX_INFERENCE_THREADS);
        assert_eq!(wild.analysis_max_dim, 640);
        assert_eq!(wild.video_max_frames, 1);
    }

    #[test]
    fn detector_input_size_is_rounded_to_the_stride() {
        let settings = AppSettings {
            detection_input_size: 700,
            ..Default::default()
        }
        .sanitised();
        assert_eq!(settings.detection_input_size % 32, 0);
        assert_eq!(settings.detection_input_size, 672);
    }

    #[test]
    fn settings_round_trip_through_the_database() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(
            AppSettings::load(&db).unwrap().recognition_threshold,
            AppSettings::default().recognition_threshold
        );

        let custom = AppSettings {
            recognition_threshold: 0.61,
            video_enabled: false,
            ..Default::default()
        };
        custom.save(&db).unwrap();

        let loaded = AppSettings::load(&db).unwrap();
        assert!((loaded.recognition_threshold - 0.61).abs() < 1e-6);
        assert!(!loaded.video_enabled);
    }

    #[test]
    fn legacy_permissive_recognition_defaults_are_tightened_on_load() {
        let db = Database::open_in_memory().unwrap();
        let legacy = AppSettings {
            recognition_threshold: LEGACY_RECOGNITION_THRESHOLD,
            recognition_margin: LEGACY_RECOGNITION_MARGIN,
            ..Default::default()
        };
        legacy.save(&db).unwrap();

        let loaded = AppSettings::load(&db).unwrap();
        assert_eq!(loaded.recognition_threshold, 0.55);
        assert_eq!(loaded.recognition_margin, 0.10);
    }

    #[test]
    fn derived_configs_track_the_settings() {
        let settings = AppSettings {
            recognition_threshold: 0.55,
            cluster_min_size: 7,
            ..Default::default()
        };
        assert!((settings.matcher_config().threshold - 0.55).abs() < 1e-6);
        assert_eq!(settings.cluster_config().min_cluster_size, 7);
    }
}
