//! Model discovery (§14).
//!
//! The application is not bound to one model. Any ONNX detector and any ONNX
//! embedder dropped into the models folder can be selected; the pipeline only
//! knows the [`FaceDetector`](skwad_face_detection::FaceDetector) and
//! [`FaceEmbedder`](skwad_face_recognition::FaceEmbedder) traits. Models are not
//! bundled — they are fetched by `scripts/fetch-models.ps1` — so this module
//! also has to describe *absence* clearly enough for the UI to explain it.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelRole {
    Detector,
    Embedder,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub role: ModelRole,
}

/// Filename fragments that identify a detector. Covers the SCRFD and
/// RetinaFace families named in the plan.
const DETECTOR_HINTS: &[&str] = &["scrfd", "retinaface", "det_", "detection", "yunet", "_det"];

/// Fragments that identify a recognition/embedding model.
const EMBEDDER_HINTS: &[&str] = &[
    "arcface", "w600k", "glint", "recognition", "_rec", "mobileface", "r50", "r100", "webface",
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub models_directory: String,
    pub available: Vec<ModelInfo>,
    pub detector: Option<String>,
    pub embedder: Option<String>,
    /// True when both a detector and an embedder are resolvable — i.e. the
    /// face pipeline can actually run.
    pub ready: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    directory: PathBuf,
}

impl ModelRegistry {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into() }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Every `.onnx` file in the models folder, classified by filename.
    pub fn list(&self) -> Vec<ModelInfo> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };

        let mut models: Vec<ModelInfo> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("onnx")))
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                ModelInfo {
                    role: classify(&name),
                    size_bytes: entry.metadata().map(|m| m.len()).unwrap_or(0),
                    path: entry.path().display().to_string(),
                    name,
                }
            })
            .collect();

        models.sort_by(|a, b| a.name.cmp(&b.name));
        models
    }

    /// Resolves a model for `role`: the explicitly chosen file if it exists,
    /// otherwise the largest candidate — larger SCRFD and ArcFace variants are
    /// consistently the more accurate ones.
    pub fn resolve(&self, role: ModelRole, preferred: Option<&str>) -> Option<PathBuf> {
        let available = self.list();

        if let Some(name) = preferred.map(|n| n.trim()).filter(|n| !n.is_empty()) {
            if let Some(found) = available.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
                return Some(PathBuf::from(&found.path));
            }
            // A model named in settings that has since been deleted should not
            // silently fall back to a different one without a trace.
            tracing::warn!(model = name, "configured model not found; falling back to auto-selection");
        }

        available
            .iter()
            .filter(|m| m.role == role)
            .max_by_key(|m| m.size_bytes)
            .map(|m| PathBuf::from(&m.path))
    }

    pub fn status(&self, preferred_detector: Option<&str>, preferred_embedder: Option<&str>) -> ModelStatus {
        let available = self.list();
        let detector = self.resolve(ModelRole::Detector, preferred_detector);
        let embedder = self.resolve(ModelRole::Embedder, preferred_embedder);
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
            detector: detector.map(|p| p.display().to_string()),
            embedder: embedder.map(|p| p.display().to_string()),
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
        let (_dir, registry) = registry_with(&[
            ("det_10g.onnx", 10),
            ("w600k_r50.onnx", 20),
            ("readme.txt", 5),
        ]);
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
    }

    #[test]
    fn a_missing_models_directory_is_not_an_error() {
        let registry = ModelRegistry::new("Z:\\definitely-not-here");
        assert!(registry.list().is_empty());
        assert!(!registry.status(None, None).ready);
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
