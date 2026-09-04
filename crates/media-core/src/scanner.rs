//! Walks a shoot folder and reports the media inside it (§3.2).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use walkdir::WalkDir;

use crate::formats::{self, Decoder, MediaKind};
use crate::{MediaError, Result};

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub filename: String,
    pub extension: String,
    pub kind: MediaKind,
    pub decoder: Decoder,
    pub file_size: u64,
    /// Identifies the *contents* cheaply: path, size and mtime hashed together.
    /// If this changes, everything derived from the file is stale.
    pub content_key: String,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub recursive: bool,
    /// Skip files that are almost certainly not shoot media (sidecars,
    /// contact sheets exported by other tools).
    pub follow_symlinks: bool,
    pub max_depth: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { recursive: true, follow_symlinks: false, max_depth: 32 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub files: Vec<ScannedFile>,
    pub photos: usize,
    pub videos: usize,
    pub skipped: usize,
    pub cancelled: bool,
}

/// Directories other tools scatter around a shoot that never contain originals.
const IGNORED_DIRS: &[&str] = &[
    ".git", "node_modules", "__MACOSX", ".Trash", "$RECYCLE.BIN",
    "Lightroom Catalog Previews.lrdata", ".thumbnails", "_skwad_export",
];

fn is_ignored_dir(name: &str) -> bool {
    IGNORED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// Indexes `root`, calling `on_progress` with the running file count so the UI
/// can show something during a long walk.
pub fn scan(
    root: &Path,
    options: &ScanOptions,
    cancel: Option<Arc<AtomicBool>>,
    mut on_progress: impl FnMut(usize),
) -> Result<ScanReport> {
    if !root.is_dir() {
        return Err(MediaError::NotFound(root.display().to_string()));
    }

    let mut report = ScanReport::default();
    let walker = WalkDir::new(root)
        .follow_links(options.follow_symlinks)
        .max_depth(if options.recursive { options.max_depth } else { 1 })
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && is_ignored_dir(&e.file_name().to_string_lossy())));

    for entry in walker {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            report.cancelled = true;
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // An unreadable subfolder should not abandon the whole import.
                tracing::warn!(error = %e, "skipping unreadable entry");
                report.skipped += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let Some((kind, decoder)) = formats::classify(path) else {
            report.skipped += 1;
            continue;
        };

        let Ok(fs_meta) = entry.metadata() else {
            report.skipped += 1;
            continue;
        };

        let file = ScannedFile {
            filename: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
            extension: formats::extension(path),
            kind,
            decoder,
            file_size: fs_meta.len(),
            content_key: content_key(path, fs_meta.len(), &fs_meta),
            modified_at: crate::metadata::file_modified_at(path),
            path: path.to_path_buf(),
        };

        match kind {
            MediaKind::Photo => report.photos += 1,
            MediaKind::Video => report.videos += 1,
        }
        report.files.push(file);

        if report.files.len() % 100 == 0 {
            on_progress(report.files.len());
        }
    }

    on_progress(report.files.len());
    Ok(report)
}

/// Hashes identity rather than content. Reading every byte of a 2,400-file
/// shoot to detect changes would cost more than the analysis that follows;
/// path + size + mtime catches the cases that matter (a file replaced or
/// re-exported in place).
fn content_key(path: &Path, size: u64, fs_meta: &std::fs::Metadata) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(&size.to_le_bytes());
    if let Ok(modified) = fs_meta.modified() {
        if let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
            hasher.update(&since_epoch.as_secs().to_le_bytes());
        }
    }
    hasher.finalize().to_hex()[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("IMG_0001.jpg"), b"not really a jpeg").unwrap();
        fs::write(dir.path().join("IMG_0002.PNG"), b"not really a png").unwrap();
        fs::write(dir.path().join("clip.mp4"), b"not really a video").unwrap();
        fs::write(dir.path().join("notes.txt"), b"ignore me").unwrap();

        let nested = dir.path().join("Day 2");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("IMG_0003.jpeg"), b"nested").unwrap();

        let ignored = dir.path().join("__MACOSX");
        fs::create_dir(&ignored).unwrap();
        fs::write(ignored.join("IMG_0004.jpg"), b"should not appear").unwrap();

        dir
    }

    #[test]
    fn finds_media_recursively_and_skips_the_rest() {
        let dir = fixture();
        let report = scan(dir.path(), &ScanOptions::default(), None, |_| {}).unwrap();

        assert_eq!(report.photos, 3, "two at the root plus one nested");
        assert_eq!(report.videos, 1);
        assert!(report.files.iter().all(|f| f.filename != "notes.txt"));
        assert!(
            report.files.iter().all(|f| f.filename != "IMG_0004.jpg"),
            "__MACOSX must be skipped entirely"
        );
    }

    #[test]
    fn non_recursive_stays_at_the_top_level() {
        let dir = fixture();
        let options = ScanOptions { recursive: false, ..Default::default() };
        let report = scan(dir.path(), &options, None, |_| {}).unwrap();
        assert_eq!(report.photos, 2);
        assert_eq!(report.videos, 1);
    }

    #[test]
    fn content_key_changes_when_the_file_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        fs::write(&path, b"one").unwrap();
        let before = scan(dir.path(), &ScanOptions::default(), None, |_| {}).unwrap().files[0]
            .content_key
            .clone();

        // Same path, different size.
        fs::write(&path, b"one and then some more bytes").unwrap();
        let after = scan(dir.path(), &ScanOptions::default(), None, |_| {}).unwrap().files[0]
            .content_key
            .clone();

        assert_ne!(before, after);
    }

    #[test]
    fn cancellation_stops_the_walk() {
        let dir = fixture();
        let cancel = Arc::new(AtomicBool::new(true));
        let report = scan(dir.path(), &ScanOptions::default(), Some(cancel), |_| {}).unwrap();
        assert!(report.cancelled);
        assert!(report.files.is_empty());
    }

    #[test]
    fn missing_folder_is_an_error() {
        assert!(scan(Path::new("Z:\\definitely-not-here"), &ScanOptions::default(), None, |_| {}).is_err());
    }
}
