//! User-facing settings, in two halves.
//!
//! Every threshold the recognition pipeline uses is here rather than baked into
//! the algorithms — §15 requires recognition thresholds to stay configurable,
//! and in practice a shoot with heavy stage lighting needs different numbers
//! from a clean studio session.
//!
//! ## Why two structs
//!
//! Once more than one machine works on a library, its settings fall into two
//! kinds that must behave in opposite ways:
//!
//! * [`LibrarySettings`] — thresholds, sampling, clustering — change the
//!   embeddings and clusters written into the shared library. Every worker
//!   must use the same values or it silently corrupts consistency. They live
//!   in the database and are authoritative for the whole fleet.
//! * [`MachineSettings`] — accelerator, threads, tool paths, model files —
//!   describe *this* installation. There is no single `accelerator` value that
//!   is right for a DirectML server and a GPU-less laptop, so these live in a
//!   local file beside the machine's other state and never travel.
//!
//! [`AppSettings`] is the flat union the pipeline, the commands and the UI
//! consume; it exists so nothing downstream had to learn the split. Before
//! this split the whole union sat in the database under one key, so loading
//! seeds a machine file from that blob the first time and the blob is written
//! back library-only from then on.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skwad_clustering::{ClusterConfig, MatcherConfig};
use skwad_database::{repo::settings, Database, Result as DbResult};
use skwad_face_detection::{Accelerator, DetectorConfig, SessionConfig};
use skwad_video_analysis::VideoAnalysisConfig;

/// The database key. Deliberately unchanged from the single-blob days so an
/// existing library's thresholds carry over without a data migration.
const KEY: &str = "app_settings";
/// Legacy setting retained for saved configuration compatibility. Runtime
/// concurrency now uses `ai_workers` plus a separate I/O worker.
const MAX_BACKGROUND_WORKERS: usize = 2;
/// Independent model pairs; keep concurrency explicit and bounded. The upper
/// limit is intentionally generous for workstation benchmarking; the default
/// remains conservative because every active worker owns its own model pair.
pub const MAX_AI_WORKERS: usize = 10;
/// Leave CPU capacity for the webview, the database client and image decoding
/// even on large workstations. ONNX inference scales poorly beyond this
/// per-session limit.
const MAX_INFERENCE_THREADS: usize = 4;
const LEGACY_RECOGNITION_THRESHOLD: f32 = 0.42;
const LEGACY_RECOGNITION_MARGIN: f32 = 0.05;

/// The file name for the machine half, under the installation's local state
/// directory.
pub const MACHINE_SETTINGS_FILE: &str = "machine-settings.json";

/// Settings that must be identical on every machine working on a library,
/// because they change what gets written into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LibrarySettings {
    // --- Detection --------------------------------------------------------
    pub detection_threshold: f32,
    pub detection_nms_threshold: f32,
    pub detection_input_size: u32,
    pub max_faces_per_image: usize,
    /// Longest edge an image is resized to before detection. Running AI on a
    /// resized copy is the single biggest performance lever (§19) — and it
    /// changes the embeddings, so it is library-wide.
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
}

impl Default for LibrarySettings {
    fn default() -> Self {
        Self {
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
        }
    }
}

impl LibrarySettings {
    pub fn load(db: &Database) -> DbResult<Self> {
        let mut conn = db.conn()?;
        // Unknown keys — the machine half, in a blob written before the split
        // — are ignored by serde, which is exactly the compatibility step.
        let mut loaded: Self = settings::get(&mut conn, KEY, LibrarySettings::default())?;
        // Upgrade installations that still carry the original permissive
        // pair. Custom values are respected; only the exact legacy defaults
        // are migrated.
        if (loaded.recognition_threshold - LEGACY_RECOGNITION_THRESHOLD).abs() < f32::EPSILON
            && (loaded.recognition_margin - LEGACY_RECOGNITION_MARGIN).abs() < f32::EPSILON
        {
            loaded.recognition_threshold = LibrarySettings::default().recognition_threshold;
            loaded.recognition_margin = LibrarySettings::default().recognition_margin;
            settings::set(&mut conn, KEY, &loaded)?;
        }
        Ok(loaded)
    }

    pub fn save(&self, db: &Database) -> DbResult<()> {
        let mut conn = db.conn()?;
        settings::set(&mut conn, KEY, self)
    }
}

/// Settings that describe this installation's hardware and tools. Never
/// shared: the right value differs from machine to machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MachineSettings {
    // --- AI runtime -------------------------------------------------------
    pub accelerator: Accelerator,
    /// Threads per inference session.
    pub inference_threads: usize,
    /// Legacy background-worker count; retained for settings compatibility.
    pub worker_threads: usize,
    /// Concurrent media analysis jobs, in addition to the indexing worker.
    pub ai_workers: usize,
    /// Overlap short-video frame preparation and use bounded parallel decode
    /// segments for long videos.
    pub video_frame_prefetch: bool,
    /// Explicit FFmpeg location, for installs that are not on `PATH`.
    pub ffmpeg_directory: Option<String>,

    // --- Models -----------------------------------------------------------
    pub detector_model: Option<String>,
    pub embedder_model: Option<String>,
}

impl Default for MachineSettings {
    fn default() -> Self {
        let cores = num_cpus::get();
        let worker_threads = cores.clamp(1, MAX_BACKGROUND_WORKERS);
        let ai_workers = cores.clamp(1, 2);
        let background_cores = cores.saturating_sub(1).max(1);
        Self {
            accelerator: Accelerator::Auto,
            // Threads are shared out across the workers rather than handed to
            // each in full. The previous default gave every worker `cores / 2`,
            // so on a 16-core machine four workers asked for 32 threads and
            // spent much of their time fighting each other for cores.
            inference_threads: (background_cores / ai_workers).clamp(1, MAX_INFERENCE_THREADS),
            worker_threads,
            ai_workers,
            video_frame_prefetch: true,
            ffmpeg_directory: None,
            detector_model: None,
            embedder_model: None,
        }
    }
}

impl MachineSettings {
    /// Reads the machine file. `None` when there is none yet — the caller
    /// then seeds one from the pre-split database blob.
    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(settings) => Some(settings),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "machine settings file is unreadable; using defaults");
                None
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// The machine half of a blob written before the split, for seeding the
    /// first machine file so nobody's accelerator choice is lost.
    fn from_legacy_blob(db: &Database) -> DbResult<Option<Self>> {
        let mut conn = db.conn()?;
        let raw: Option<serde_json::Value> = settings::get_opt(&mut conn, KEY)?;
        Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
    }
}

/// The flat union every consumer sees. `library` and `machine` split it back
/// out for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    // --- AI runtime (machine) --------------------------------------------
    pub accelerator: Accelerator,
    pub inference_threads: usize,
    pub worker_threads: usize,
    pub ai_workers: usize,

    // --- Detection (library) ---------------------------------------------
    pub detection_threshold: f32,
    pub detection_nms_threshold: f32,
    pub detection_input_size: u32,
    pub max_faces_per_image: usize,
    pub analysis_max_dim: u32,

    // --- Recognition (library) -------------------------------------------
    pub recognition_threshold: f32,
    pub recognition_margin: f32,
    pub unique_person_per_frame: bool,
    pub auto_confirm_above: f32,

    // --- Clustering (library) --------------------------------------------
    pub cluster_edge_threshold: f32,
    pub cluster_min_size: usize,
    pub cluster_merge_threshold: f32,
    pub cluster_neighbours: usize,

    // --- Video (library, except prefetch) --------------------------------
    pub video_enabled: bool,
    pub video_scene_threshold: f64,
    pub video_sample_interval: f64,
    pub video_max_frames: usize,
    pub video_frame_prefetch: bool,

    // --- Scanning (library) and tools (machine) --------------------------
    pub scan_recursive: bool,
    pub ffmpeg_directory: Option<String>,

    // --- Models (machine) ------------------------------------------------
    pub detector_model: Option<String>,
    pub embedder_model: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::compose(LibrarySettings::default(), MachineSettings::default())
    }
}

impl AppSettings {
    /// Joins the two halves into the flat view.
    pub fn compose(library: LibrarySettings, machine: MachineSettings) -> Self {
        Self {
            accelerator: machine.accelerator,
            inference_threads: machine.inference_threads,
            worker_threads: machine.worker_threads,
            ai_workers: machine.ai_workers,
            detection_threshold: library.detection_threshold,
            detection_nms_threshold: library.detection_nms_threshold,
            detection_input_size: library.detection_input_size,
            max_faces_per_image: library.max_faces_per_image,
            analysis_max_dim: library.analysis_max_dim,
            recognition_threshold: library.recognition_threshold,
            recognition_margin: library.recognition_margin,
            unique_person_per_frame: library.unique_person_per_frame,
            auto_confirm_above: library.auto_confirm_above,
            cluster_edge_threshold: library.cluster_edge_threshold,
            cluster_min_size: library.cluster_min_size,
            cluster_merge_threshold: library.cluster_merge_threshold,
            cluster_neighbours: library.cluster_neighbours,
            video_enabled: library.video_enabled,
            video_scene_threshold: library.video_scene_threshold,
            video_sample_interval: library.video_sample_interval,
            video_max_frames: library.video_max_frames,
            video_frame_prefetch: machine.video_frame_prefetch,
            scan_recursive: library.scan_recursive,
            ffmpeg_directory: machine.ffmpeg_directory,
            detector_model: machine.detector_model,
            embedder_model: machine.embedder_model,
        }
    }

    /// The half that must be identical on every machine.
    pub fn library(&self) -> LibrarySettings {
        LibrarySettings {
            detection_threshold: self.detection_threshold,
            detection_nms_threshold: self.detection_nms_threshold,
            detection_input_size: self.detection_input_size,
            max_faces_per_image: self.max_faces_per_image,
            analysis_max_dim: self.analysis_max_dim,
            recognition_threshold: self.recognition_threshold,
            recognition_margin: self.recognition_margin,
            unique_person_per_frame: self.unique_person_per_frame,
            auto_confirm_above: self.auto_confirm_above,
            cluster_edge_threshold: self.cluster_edge_threshold,
            cluster_min_size: self.cluster_min_size,
            cluster_merge_threshold: self.cluster_merge_threshold,
            cluster_neighbours: self.cluster_neighbours,
            video_enabled: self.video_enabled,
            video_scene_threshold: self.video_scene_threshold,
            video_sample_interval: self.video_sample_interval,
            video_max_frames: self.video_max_frames,
            scan_recursive: self.scan_recursive,
        }
    }

    /// The half that belongs to this installation.
    pub fn machine(&self) -> MachineSettings {
        MachineSettings {
            accelerator: self.accelerator,
            inference_threads: self.inference_threads,
            worker_threads: self.worker_threads,
            ai_workers: self.ai_workers,
            video_frame_prefetch: self.video_frame_prefetch,
            ffmpeg_directory: self.ffmpeg_directory.clone(),
            detector_model: self.detector_model.clone(),
            embedder_model: self.embedder_model.clone(),
        }
    }

    /// Loads both halves: the library half from the database, the machine
    /// half from `machine_file`. A machine with no file yet — every machine
    /// the first time this build runs — is seeded from the machine keys the
    /// pre-split blob carried, so an accelerator or FFmpeg path someone set
    /// survives the split. The database blob itself is left alone until the
    /// next save, which writes it back library-only.
    pub fn load(db: &Database, machine_file: &Path) -> DbResult<Self> {
        let library = LibrarySettings::load(db)?;
        let machine = match MachineSettings::load(machine_file) {
            Some(machine) => machine,
            None => {
                let seeded = MachineSettings::from_legacy_blob(db)?.unwrap_or_default();
                if let Err(error) = seeded.save(machine_file) {
                    tracing::warn!(%error, path = %machine_file.display(), "could not write the machine settings file");
                }
                seeded
            }
        };
        Ok(Self::compose(library, machine))
    }

    /// Persists both halves to their own homes.
    pub fn save(&self, db: &Database, machine_file: &Path) -> DbResult<()> {
        self.library().save(db)?;
        self.machine()
            .save(machine_file)
            .map_err(|e| skwad_database::DbError::other(format!("could not save machine settings: {e}")))
    }

    /// Clamps every value into a range the pipeline can actually work with.
    /// Settings arrive from the UI and from a JSON blob written by an older
    /// build, so neither source is trusted.
    pub fn sanitised(mut self) -> Self {
        let cores = num_cpus::get();
        self.worker_threads = self.worker_threads.clamp(1, cores.clamp(1, MAX_BACKGROUND_WORKERS));
        self.ai_workers = self.ai_workers.clamp(1, cores.clamp(1, MAX_AI_WORKERS));
        let background_cores = cores.saturating_sub(1).max(1);
        let max_threads = (background_cores / self.ai_workers).clamp(1, MAX_INFERENCE_THREADS);
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

/// Where this installation keeps its machine half.
pub fn machine_settings_path(local_state_dir: &Path) -> PathBuf {
    local_state_dir.join(MACHINE_SETTINGS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = machine_settings_path(dir.path());
        (dir, path)
    }

    #[test]
    fn old_settings_enable_two_ai_workers_and_new_values_are_bounded() {
        let settings: AppSettings = serde_json::from_str(r#"{"workerThreads":2}"#).unwrap();
        assert_eq!(settings.ai_workers, num_cpus::get().clamp(1, 2));
        let mut settings = settings;
        settings.ai_workers = usize::MAX;
        let settings = settings.sanitised();
        assert!(settings.ai_workers <= MAX_AI_WORKERS);
        assert!(settings.ai_workers * settings.inference_threads <= num_cpus::get().max(1));
    }

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
            settings.ai_workers * settings.inference_threads <= cores.max(1),
            "{} workers x {} threads exceeds {cores} cores",
            settings.ai_workers,
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

    /// The split must be lossless: every field lands in exactly one half and
    /// comes back through `compose`.
    #[test]
    fn the_two_halves_recompose_to_the_flat_view() {
        let original = AppSettings {
            accelerator: Accelerator::Cpu,
            ai_workers: 3,
            recognition_threshold: 0.61,
            video_frame_prefetch: false,
            ffmpeg_directory: Some("C:\\ffmpeg".into()),
            embedder_model: Some("w600k_r50.onnx".into()),
            scan_recursive: false,
            ..Default::default()
        };
        let recomposed = AppSettings::compose(original.library(), original.machine());
        assert_eq!(
            serde_json::to_value(&recomposed).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
    }

    #[test]
    fn settings_round_trip_through_their_two_homes() {
        let db = Database::open_test().unwrap();
        let (_dir, machine_file) = machine_file();
        assert_eq!(
            AppSettings::load(&db, &machine_file).unwrap().recognition_threshold,
            AppSettings::default().recognition_threshold
        );

        let custom = AppSettings {
            recognition_threshold: 0.61,
            video_enabled: false,
            accelerator: Accelerator::Cpu,
            ffmpeg_directory: Some("D:\\tools\\ffmpeg".into()),
            ..Default::default()
        };
        custom.save(&db, &machine_file).unwrap();

        let loaded = AppSettings::load(&db, &machine_file).unwrap();
        assert!((loaded.recognition_threshold - 0.61).abs() < 1e-6);
        assert!(!loaded.video_enabled);
        assert_eq!(loaded.accelerator, Accelerator::Cpu);
        assert_eq!(loaded.ffmpeg_directory.as_deref(), Some("D:\\tools\\ffmpeg"));

        // The database blob carries only the library half now: a second
        // machine reading it must not inherit this one's accelerator.
        let mut conn = db.conn().unwrap();
        let blob: serde_json::Value = settings::get_opt(&mut conn, KEY).unwrap().unwrap();
        assert!(blob.get("recognitionThreshold").is_some());
        assert!(blob.get("accelerator").is_none(), "machine keys stay out of the library");
        assert!(blob.get("ffmpegDirectory").is_none());
        assert!(machine_file.is_file());
    }

    /// A library written by a build before the split has every key in one
    /// blob. The first load on each machine must seed its file from that, so
    /// nobody's accelerator choice silently reverts to Auto.
    #[test]
    fn a_pre_split_blob_seeds_the_machine_file_once() {
        let db = Database::open_test().unwrap();
        let (_dir, machine_file) = machine_file();
        {
            let mut conn = db.conn().unwrap();
            settings::set(
                &mut conn,
                KEY,
                &serde_json::json!({
                    "accelerator": "cpu",
                    "aiWorkers": 4,
                    "ffmpegDirectory": "E:\\ffmpeg",
                    "recognitionThreshold": 0.7
                }),
            )
            .unwrap();
        }

        let loaded = AppSettings::load(&db, &machine_file).unwrap();
        assert_eq!(loaded.accelerator, Accelerator::Cpu);
        assert_eq!(loaded.ai_workers, 4);
        assert_eq!(loaded.ffmpeg_directory.as_deref(), Some("E:\\ffmpeg"));
        assert!((loaded.recognition_threshold - 0.7).abs() < 1e-6);
        assert!(machine_file.is_file(), "seeded on first load");

        // Once the file exists it wins, even if the blob still carries the
        // old machine keys.
        let mut on_disk = MachineSettings::load(&machine_file).unwrap();
        on_disk.ai_workers = 1;
        on_disk.save(&machine_file).unwrap();
        assert_eq!(AppSettings::load(&db, &machine_file).unwrap().ai_workers, 1);
    }

    #[test]
    fn legacy_permissive_recognition_defaults_are_tightened_on_load() {
        let db = Database::open_test().unwrap();
        let (_dir, machine_file) = machine_file();
        let legacy = AppSettings {
            recognition_threshold: LEGACY_RECOGNITION_THRESHOLD,
            recognition_margin: LEGACY_RECOGNITION_MARGIN,
            ..Default::default()
        };
        legacy.save(&db, &machine_file).unwrap();

        let loaded = AppSettings::load(&db, &machine_file).unwrap();
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
