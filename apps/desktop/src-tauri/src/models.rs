//! Model discovery (§14) and identity.
//!
//! The application is not bound to one model. Any ONNX detector and any ONNX
//! embedder dropped into the models folder can be selected; the pipeline only
//! knows the [`FaceDetector`](skwad_face_detection::FaceDetector) and
//! [`FaceEmbedder`](skwad_face_recognition::FaceEmbedder) traits.
//!
//! Models may or may not be bundled. A build made with
//! `src-tauri/tauri.models.conf.json` carries them and [`seed_from_bundle`]
//! installs them on first launch; a default build does not, and they are
//! fetched per machine by `scripts/fetch-models.ps1`. Either way this module
//! has to describe *absence* clearly enough for the UI to explain it.
//!
//! ## Identity
//!
//! Filenames are hints, not identities. Two machines can each hold a file
//! matching `w600k_r50` that are different weights, producing embeddings in
//! different vector spaces — and nothing errors, recognition just quietly
//! degrades. So every model also carries a content hash, computed once and
//! cached against `(size, mtime)` the way `content_key` is for media, and every
//! embedding written to the database records the hash of the embedder that
//! produced it (`faces.model_key`). Vectors are only ever compared within one
//! cohort, and a worker whose hashes differ from the library's is refused work.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Where a models-bundled build puts them, relative to the resource directory.
const BUNDLED_MODELS: &str = "models";

/// The hash cache, beside the models it describes. Rehashing 183 MB on every
/// registry call would be noticeable; a `(size, mtime)` check is not.
const HASH_CACHE: &str = ".model-hashes.json";

/// Copies bundled models into the library's models folder, once.
///
/// Returns the names it installed. An empty result is the normal, quiet case:
/// either this build carries no models, or they are already in place.
///
/// Deliberately conservative about what it overwrites. A file that is already
/// there and already the right size is left alone — someone may have put their
/// own model there on purpose, and re-copying 174 MB on every launch would be
/// wasteful besides. A file that exists at the *wrong* size is replaced,
/// because that is what a copy interrupted by a crash or a full disk looks
/// like, and ONNX Runtime's failure on a truncated model is not obviously a
/// storage problem.
pub fn seed_from_bundle(app: &AppHandle, destination: &Path) -> Vec<String> {
    let Ok(bundled) = app.path().resolve(BUNDLED_MODELS, tauri::path::BaseDirectory::Resource) else {
        return Vec::new();
    };
    seed_from_directory(&bundled, destination)
}

/// The Tauri-free half of [`seed_from_bundle`]: copies every `.onnx` under
/// `bundled` into `destination` that is missing or the wrong size.
pub fn seed_from_directory(bundled: &Path, destination: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(bundled) else {
        // A build without models: nothing to say.
        return Vec::new();
    };

    if let Err(error) = std::fs::create_dir_all(destination) {
        tracing::warn!(%error, directory = %destination.display(), "could not create the models folder");
        return Vec::new();
    }

    let mut installed = Vec::new();
    for entry in entries.flatten() {
        let source = entry.path();
        if !source.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("onnx")) {
            continue;
        }
        let Some(name) = source.file_name() else { continue };
        let target = destination.join(name);

        let source_len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::metadata(&target).is_ok_and(|m| m.len() == source_len) {
            continue;
        }

        // Copy beside the target and rename into place, so an interrupted copy
        // never leaves a half-written model where the registry will find it and
        // hand it to ONNX Runtime.
        let staging = destination.join(format!("{}.partial", name.to_string_lossy()));
        match std::fs::copy(&source, &staging).and_then(|_| std::fs::rename(&staging, &target)) {
            Ok(_) => {
                tracing::info!(model = %name.to_string_lossy(), bytes = source_len, "installed a bundled model");
                installed.push(name.to_string_lossy().into_owned());
            }
            Err(error) => {
                tracing::warn!(%error, model = %name.to_string_lossy(), "could not install a bundled model");
                let _ = std::fs::remove_file(&staging);
            }
        }
    }
    installed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelRole {
    Detector,
    Embedder,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub role: ModelRole,
    /// BLAKE3 of the file's contents, hex. The model's identity everywhere
    /// else in the system: `faces.model_key`, the worker handshake, the
    /// download URL.
    pub hash: String,
}

/// Filename fragments that identify a detector. Covers the SCRFD and
/// RetinaFace families named in the plan.
const DETECTOR_HINTS: &[&str] = &["scrfd", "retinaface", "det_", "detection", "yunet", "_det"];

/// Fragments that identify a recognition/embedding model.
const EMBEDDER_HINTS: &[&str] = &[
    "arcface",
    "w600k",
    "glint",
    "recognition",
    "_rec",
    "mobileface",
    "r50",
    "r100",
    "webface",
];

pub fn classify(file_name: &str) -> ModelRole {
    let lower = file_name.to_ascii_lowercase();
    // Detector hints are checked first: "det_10g" would otherwise be caught by
    // nothing, while "w600k_r50" matches an embedder hint either way.
    if DETECTOR_HINTS.iter().any(|h| lower.contains(h)) {
        ModelRole::Detector
    } else if EMBEDDER_HINTS.iter().any(|h| lower.contains(h)) {
        ModelRole::Embedder
    } else {
        ModelRole::Unknown
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub models_directory: String,
    pub available: Vec<ModelInfo>,
    pub detector: Option<String>,
    pub embedder: Option<String>,
    /// Content hashes of the resolved pair — what a worker must match.
    pub detector_hash: Option<String>,
    pub embedder_hash: Option<String>,
    /// True when both a detector and an embedder are resolvable — i.e. the
    /// face pipeline can actually run.
    pub ready: bool,
    pub message: String,
}

/// One cached hash: valid while the file's size and mtime are unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CachedHash {
    size: u64,
    modified: u64,
    hash: String,
}

fn modified_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Hashes a whole file with BLAKE3, streaming so a 170 MB model never sits on
/// the heap in one piece. Runs at memory bandwidth; a full model pair takes
/// well under a second, and it only happens when a file changes.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    directory: PathBuf,
}

impl ModelRegistry {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn cache_path(&self) -> PathBuf {
        self.directory.join(HASH_CACHE)
    }

    fn load_cache(&self) -> HashMap<String, CachedHash> {
        std::fs::read(self.cache_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn store_cache(&self, cache: &HashMap<String, CachedHash>) {
        if let Ok(json) = serde_json::to_vec_pretty(cache) {
            if let Err(error) = std::fs::write(self.cache_path(), json) {
                tracing::debug!(%error, "could not write the model hash cache; models will be rehashed next time");
            }
        }
    }

    /// Every `.onnx` file in the models folder, classified by filename and
    /// identified by content hash.
    pub fn list(&self) -> Vec<ModelInfo> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };

        let mut cache = self.load_cache();
        let mut cache_changed = false;

        let mut models: Vec<ModelInfo> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("onnx")))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let meta = entry.metadata().ok()?;
                let (size, modified) = (meta.len(), modified_secs(&meta));
                let hash = match cache.get(&name) {
                    Some(cached) if cached.size == size && cached.modified == modified => cached.hash.clone(),
                    _ => {
                        let hash = match hash_file(&entry.path()) {
                            Ok(hash) => hash,
                            Err(error) => {
                                tracing::warn!(model = %name, %error, "could not hash a model; it will be skipped");
                                return None;
                            }
                        };
                        cache.insert(
                            name.clone(),
                            CachedHash {
                                size,
                                modified,
                                hash: hash.clone(),
                            },
                        );
                        cache_changed = true;
                        hash
                    }
                };
                Some(ModelInfo {
                    role: classify(&name),
                    size_bytes: size,
                    path: entry.path().display().to_string(),
                    name,
                    hash,
                })
            })
            .collect();

        // Forget files that are gone, so the cache does not grow forever.
        let present: std::collections::HashSet<&str> = models.iter().map(|m| m.name.as_str()).collect();
        let before = cache.len();
        cache.retain(|name, _| present.contains(name.as_str()));
        if cache_changed || cache.len() != before {
            self.store_cache(&cache);
        }

        models.sort_by(|a, b| a.name.cmp(&b.name));
        models
    }

    /// Resolves a model for `role`: the explicitly chosen file if it exists,
    /// otherwise the largest candidate — larger SCRFD and ArcFace variants are
    /// consistently the more accurate ones.
    pub fn resolve(&self, role: ModelRole, preferred: Option<&str>) -> Option<PathBuf> {
        self.resolve_info(role, preferred).map(|m| PathBuf::from(m.path))
    }

    /// As [`resolve`](Self::resolve), with the model's identity attached.
    pub fn resolve_info(&self, role: ModelRole, preferred: Option<&str>) -> Option<ModelInfo> {
        let available = self.list();

        if let Some(name) = preferred.map(|n| n.trim()).filter(|n| !n.is_empty()) {
            if let Some(found) = available.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
                return Some(found.clone());
            }
            // A model named in settings that has since been deleted should not
            // silently fall back to a different one without a trace.
            tracing::warn!(
                model = name,
                "configured model not found; falling back to auto-selection"
            );
        }

        available
            .into_iter()
            .filter(|m| m.role == role)
            .max_by_key(|m| m.size_bytes)
    }

    /// Finds a model by content hash — how a worker asks the server for the
    /// exact file the library was embedded with.
    pub fn find_by_hash(&self, hash: &str) -> Option<ModelInfo> {
        self.list().into_iter().find(|m| m.hash.eq_ignore_ascii_case(hash))
    }

    pub fn status(&self, preferred_detector: Option<&str>, preferred_embedder: Option<&str>) -> ModelStatus {
        let available = self.list();
        let detector = self.resolve_info(ModelRole::Detector, preferred_detector);
        let embedder = self.resolve_info(ModelRole::Embedder, preferred_embedder);
        let ready = detector.is_some() && embedder.is_some();

        let message = if ready {
            "Face detection and recognition models are ready.".to_string()
        } else if available.is_empty() {
            format!(
                "No models found in {}. Run scripts/fetch-models.ps1 (Windows) or scripts/fetch-models.sh (macOS) to download them.",
                self.directory.display()
            )
        } else {
            let missing = match (detector.is_some(), embedder.is_some()) {
                (false, false) => "a face detector and a face embedder",
                (false, true) => "a face detector",
                _ => "a face embedder",
            };
            format!("Found {} model file(s) but still need {missing}.", available.len())
        };

        ModelStatus {
            models_directory: self.directory.display().to_string(),
            available,
            detector: detector.as_ref().map(|m| m.path.clone()),
            embedder: embedder.as_ref().map(|m| m.path.clone()),
            detector_hash: detector.map(|m| m.hash),
            embedder_hash: embedder.map(|m| m.hash),
            ready,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_detector_filenames() {
        assert_eq!(classify("scrfd_10g_bnkps.onnx"), ModelRole::Detector);
        assert_eq!(classify("det_10g.onnx"), ModelRole::Detector);
        assert_eq!(classify("RetinaFace-R50.onnx"), ModelRole::Detector);
    }

    #[test]
    fn recognises_embedder_filenames() {
        assert_eq!(classify("w600k_r50.onnx"), ModelRole::Embedder);
        assert_eq!(classify("arcface_r100.onnx"), ModelRole::Embedder);
        assert_eq!(classify("glintr100.onnx"), ModelRole::Embedder);
    }

    #[test]
    fn unknown_filenames_are_not_guessed_at() {
        assert_eq!(classify("something_else.onnx"), ModelRole::Unknown);
    }

    fn registry_with(files: &[(&str, usize)]) -> (tempdir::TempHolder, ModelRegistry) {
        let dir = tempdir::TempHolder::new();
        for (name, size) in files {
            std::fs::write(dir.path().join(name), vec![0u8; *size]).unwrap();
        }
        let registry = ModelRegistry::new(dir.path());
        (dir, registry)
    }

    #[test]
    fn lists_only_onnx_files() {
        let (_dir, registry) = registry_with(&[("det_10g.onnx", 10), ("w600k_r50.onnx", 20), ("readme.txt", 5)]);
        let listed = registry.list();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|m| m.name.ends_with(".onnx")));
    }

    #[test]
    fn prefers_the_larger_model_of_a_role() {
        let (_dir, registry) = registry_with(&[("det_500m.onnx", 10), ("det_10g.onnx", 500)]);
        let resolved = registry.resolve(ModelRole::Detector, None).unwrap();
        assert!(resolved.ends_with("det_10g.onnx"));
    }

    #[test]
    fn an_explicit_choice_wins() {
        let (_dir, registry) = registry_with(&[("det_500m.onnx", 10), ("det_10g.onnx", 500)]);
        let resolved = registry.resolve(ModelRole::Detector, Some("det_500m.onnx")).unwrap();
        assert!(resolved.ends_with("det_500m.onnx"));
    }

    #[test]
    fn a_stale_choice_falls_back_instead_of_failing() {
        let (_dir, registry) = registry_with(&[("det_10g.onnx", 500)]);
        let resolved = registry.resolve(ModelRole::Detector, Some("deleted.onnx")).unwrap();
        assert!(resolved.ends_with("det_10g.onnx"));
    }

    #[test]
    fn status_explains_an_empty_models_folder() {
        let (_dir, registry) = registry_with(&[]);
        let status = registry.status(None, None);
        assert!(!status.ready);
        assert!(status.message.contains("fetch-models"), "got: {}", status.message);
    }

    #[test]
    fn status_explains_a_half_populated_folder() {
        let (_dir, registry) = registry_with(&[("det_10g.onnx", 500)]);
        let status = registry.status(None, None);
        assert!(!status.ready);
        assert!(status.detector.is_some());
        assert!(status.embedder.is_none());
        assert!(status.message.contains("face embedder"), "got: {}", status.message);
    }

    #[test]
    fn status_is_ready_when_both_roles_resolve() {
        let (_dir, registry) = registry_with(&[("det_10g.onnx", 500), ("w600k_r50.onnx", 900)]);
        let status = registry.status(None, None);
        assert!(status.ready);
        assert!(status.detector_hash.is_some());
        assert_ne!(status.detector_hash, status.embedder_hash);
    }

    #[test]
    fn a_missing_models_directory_is_not_an_error() {
        let registry = ModelRegistry::new("Z:\\definitely-not-here");
        assert!(registry.list().is_empty());
        assert!(!registry.status(None, None).ready);
    }

    /// Identity is the content, not the name: the same bytes under two names
    /// are one model, and a file that changes gets a new hash even though its
    /// name did not.
    #[test]
    fn models_are_identified_by_content_and_the_hash_is_cached() {
        let (dir, registry) = registry_with(&[("w600k_r50.onnx", 64), ("copy_of_w600k_r50.onnx", 64)]);
        let listed = registry.list();
        assert_eq!(listed[0].hash, listed[1].hash, "same bytes, same identity");
        assert_eq!(listed[0].hash.len(), 64, "blake3 hex");
        assert!(registry.find_by_hash(&listed[0].hash).is_some());

        let cache = registry.load_cache();
        assert_eq!(cache.len(), 2, "both hashes cached on first listing");

        // A rewritten file with different contents and a bumped mtime is a
        // different model.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.path().join("w600k_r50.onnx"), vec![1u8; 64]).unwrap();
        let relisted = registry.list();
        let changed = relisted.iter().find(|m| m.name == "w600k_r50.onnx").unwrap();
        let unchanged = relisted.iter().find(|m| m.name == "copy_of_w600k_r50.onnx").unwrap();
        assert_ne!(changed.hash, unchanged.hash);
        assert_eq!(unchanged.hash, listed[1].hash);

        std::fs::remove_file(dir.path().join("copy_of_w600k_r50.onnx")).unwrap();
        registry.list();
        assert_eq!(registry.load_cache().len(), 1, "a removed model leaves the cache");
    }

    /// A minimal scratch directory that cleans itself up, so the crate does not
    /// need a dev-dependency purely for these tests.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempHolder(PathBuf);

        impl TempHolder {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "skwad-models-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                TempHolder(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempHolder {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).ok();
            }
        }
    }
}
