//! Cached still frames used by the video face reviewer.
//!
//! Video analysis already decodes these pixels. Keeping a compact JPEG copy in
//! the application cache avoids seeking through the original 4K file every
//! time a user opens a face or moves between analysed samples.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::{codecs::jpeg::JpegEncoder, RgbImage};

use crate::{MediaError, Result};

const REVIEW_FRAME_QUALITY: u8 = 88;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct VideoFrameCache {
    root: PathBuf,
}

impl VideoFrameCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, content_key: &str, timestamp: f64) -> PathBuf {
        let safe_key: String = content_key
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(80)
            .collect();
        let key = if safe_key.is_empty() { "unknown" } else { &safe_key };
        let shard = key.get(..2).unwrap_or("00");
        let milliseconds = (timestamp.max(0.0) * 1_000.0).round() as u64;
        self.root.join(shard).join(key).join(format!("{milliseconds:012}.jpg"))
    }

    pub fn read(&self, content_key: &str, timestamp: f64) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(content_key, timestamp);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(MediaError::Io(format!("read {}: {error}", path.display()))),
        }
    }

    /// Encodes a decoded frame the way [`Self::store`] would, without writing
    /// it — for a worker that hands its frames to another machine to keep.
    pub fn encode(image: &RgbImage) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, REVIEW_FRAME_QUALITY)
            .encode_image(image)
            .map_err(|error| MediaError::Io(format!("encode review frame: {error}")))?;
        Ok(bytes)
    }

    /// Writes an already-encoded review frame. Idempotent: a frame that is
    /// already cached is left alone.
    pub fn store_encoded(&self, jpeg: &[u8], content_key: &str, timestamp: f64) -> Result<()> {
        let target = self.path_for(content_key, timestamp);
        if target.is_file() {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| MediaError::Io(format!("create {}: {error}", parent.display())))?;
        }
        let temporary = target.with_extension(format!(
            "{}.tmp",
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temporary, jpeg).map_err(|error| MediaError::Io(format!("write {}: {error}", temporary.display())))?;
        std::fs::rename(&temporary, &target).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            MediaError::Io(format!("place {}: {error}", target.display()))
        })?;
        Ok(())
    }

    pub fn store(&self, image: &RgbImage, content_key: &str, timestamp: f64) -> Result<Vec<u8>> {
        let target = self.path_for(content_key, timestamp);
        if let Some(bytes) = self.read(content_key, timestamp)? {
            return Ok(bytes);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| MediaError::Io(format!("create {}: {error}", parent.display())))?;
        }

        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, REVIEW_FRAME_QUALITY)
            .encode_image(image)
            .map_err(|error| MediaError::Encode(error.to_string()))?;

        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = target.with_extension(format!("jpg.part.{}.{}", std::process::id(), sequence));
        std::fs::write(&temporary, &bytes)
            .map_err(|error| MediaError::Io(format!("write {}: {error}", temporary.display())))?;
        match std::fs::rename(&temporary, &target) {
            Ok(()) => {}
            Err(error) if target.is_file() => {
                let _ = std::fs::remove_file(&temporary);
                tracing::debug!(file = %target.display(), %error, "review frame was cached concurrently");
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(MediaError::Io(format!("finalise {}: {error}", target.display())));
            }
        }
        Ok(bytes)
    }

    pub fn remove(&self, content_key: &str) -> Result<u64> {
        let directory = self.path_for(content_key, 0.0);
        let Some(directory) = directory.parent() else {
            return Ok(0);
        };
        if !directory.exists() {
            return Ok(0);
        }
        let removed = walkdir::WalkDir::new(directory)
            .into_iter()
            .flatten()
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
            .count() as u64;
        let _ = std::fs::remove_dir(directory);
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reads_a_frame_by_content_and_millisecond() {
        let directory = tempfile::tempdir().unwrap();
        let cache = VideoFrameCache::new(directory.path());
        let image = RgbImage::from_pixel(32, 18, image::Rgb([24, 72, 96]));

        let bytes = cache.store(&image, "ab12-cd34", 5.1234).unwrap();
        let path = cache.path_for("ab12-cd34", 5.123);

        assert!(path.starts_with(cache.root()));
        assert!(path.ends_with(Path::new("ab").join("ab12cd34").join("000000005123.jpg")));
        assert_eq!(cache.read("ab12-cd34", 5.123).unwrap(), Some(bytes));
    }

    #[test]
    fn remove_only_clears_one_video() {
        let directory = tempfile::tempdir().unwrap();
        let cache = VideoFrameCache::new(directory.path());
        let image = RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]));
        cache.store(&image, "aa-one", 0.0).unwrap();
        cache.store(&image, "aa-two", 0.0).unwrap();

        assert_eq!(cache.remove("aa-one").unwrap(), 1);
        assert!(cache.read("aa-one", 0.0).unwrap().is_none());
        assert!(cache.read("aa-two", 0.0).unwrap().is_some());
    }
}
