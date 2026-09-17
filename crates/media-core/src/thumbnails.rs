//! Content-addressed thumbnail cache.
//!
//! Thumbnails are keyed by the scanner's `content_key`, so re-importing the
//! same folder — or importing it into a second shoot — reuses the work instead
//! of redoing it. They live under the application data directory; source media
//! is never touched (§17).
//!
//! Photo thumbnails are encoded as lossy WebP (via libwebp — the `image` crate's
//! own WebP encoder is lossless-only and would not beat JPEG on size, defeating
//! the point). Video poster frames stay JPEG: they are already a single frame
//! grabbed once per video, so the extra libwebp encode time buys nothing there,
//! and keeping one path JPEG avoids a second format for every cache consumer to
//! special-case for no size benefit at video-poster volumes.

use std::path::{Path, PathBuf};

use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, RgbImage};
use webp::Encoder as WebpEncoder;

use crate::ffmpeg::Ffmpeg;
use crate::formats::{self, MediaKind};
use crate::{MediaError, Result};

/// Long edge of a generated thumbnail. Big enough for a crisp grid tile on a
/// high-DPI display, small enough that thousands of them stay cheap.
pub const THUMBNAIL_MAX_DIM: u32 = 512;

/// JPEG quality for video poster frames. 82 is the point where further
/// increases cost bytes without visibly improving a 512px tile.
const THUMBNAIL_JPEG_QUALITY: u8 = 82;

/// Lossy WebP quality for photo thumbnails, on libwebp's 0–100 scale. Chosen
/// to land visibly smaller than the JPEG equivalent at the same 512px tile
/// size without introducing visible blocking.
const THUMBNAIL_WEBP_QUALITY: f32 = 80.0;

/// Where a video's poster frame is taken from, as a fraction of its duration.
/// A little way in avoids black leader frames and slates.
const VIDEO_POSTER_FRACTION: f64 = 0.1;

#[derive(Debug, Clone)]
pub struct ThumbnailCache {
    root: PathBuf,
}

impl ThumbnailCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Fans the cache out over 256 subdirectories. A single folder holding
    /// 100,000 files is slow to enumerate on both NTFS and APFS. Photos land
    /// as `.webp`, videos as `.jpg` — see the module comment for why.
    pub fn path_for(&self, content_key: &str, kind: MediaKind) -> PathBuf {
        let shard = content_key.get(..2).unwrap_or("00");
        let extension = extension_for(kind);
        self.root.join(shard).join(format!("{content_key}.{extension}"))
    }

    pub fn exists(&self, content_key: &str, kind: MediaKind) -> bool {
        self.path_for(content_key, kind).is_file()
    }

    /// Generates the thumbnail if it is not already cached, and returns its path.
    pub fn ensure(
        &self,
        source: &Path,
        content_key: &str,
        orientation: u16,
        duration: Option<f64>,
        ffmpeg: Option<&Ffmpeg>,
    ) -> Result<PathBuf> {
        let (kind, _) =
            formats::classify(source).ok_or_else(|| MediaError::Unsupported(source.display().to_string()))?;
        let target = self.path_for(content_key, kind);
        if target.is_file() {
            return Ok(target);
        }

        let image = render_source(source, kind, orientation, duration, ffmpeg)?;
        self.write(&image, kind, &target)?;
        Ok(target)
    }

    /// Stores an already-rendered image as a thumbnail. Used by the analysis
    /// pipeline, which has the decoded pixels in hand and should not decode twice.
    pub fn store(&self, image: &RgbImage, content_key: &str, kind: MediaKind) -> Result<PathBuf> {
        let target = self.path_for(content_key, kind);
        if target.is_file() {
            return Ok(target);
        }
        self.write(image, kind, &target)?;
        Ok(target)
    }

    fn write(&self, image: &RgbImage, kind: MediaKind, target: &Path) -> Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MediaError::Io(format!("create {}: {e}", parent.display())))?;
        }

        let scaled = downscale(image, THUMBNAIL_MAX_DIM);

        // Write to a temporary file first: a half-written thumbnail left behind
        // by a crash would otherwise be cached forever as if it were valid.
        let temp = target.with_extension(format!("{}.part", extension_for(kind)));
        let buffer = encode(&scaled, kind)?;
        std::fs::write(&temp, &buffer).map_err(|e| MediaError::Io(format!("write {}: {e}", temp.display())))?;
        std::fs::rename(&temp, target).map_err(|e| MediaError::Io(format!("finalise {}: {e}", target.display())))?;
        Ok(())
    }

    /// Deletes cached thumbnails. Used by the Settings screen; costs nothing
    /// but regeneration time.
    pub fn clear(&self) -> Result<u64> {
        if !self.root.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        for entry in walkdir::WalkDir::new(&self.root).into_iter().flatten() {
            if entry.file_type().is_file() && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Total bytes on disk, for the cache-management view.
    pub fn size_on_disk(&self) -> u64 {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }
}

fn render_source(
    source: &Path,
    kind: MediaKind,
    orientation: u16,
    duration: Option<f64>,
    ffmpeg: Option<&Ffmpeg>,
) -> Result<RgbImage> {
    match kind {
        MediaKind::Photo => crate::decode::load_image(source, orientation, Some(THUMBNAIL_MAX_DIM * 2), ffmpeg),
        MediaKind::Video => {
            let ff = ffmpeg.ok_or_else(|| MediaError::MissingFfmpeg("video thumbnails need FFmpeg".into()))?;
            let at = duration
                .map(|d| (d * VIDEO_POSTER_FRACTION).clamp(0.0, (d - 0.1).max(0.0)))
                .unwrap_or(0.0);
            crate::decode::load_video_frame(source, at, orientation, Some(THUMBNAIL_MAX_DIM * 2), ff)
        }
    }
}

fn extension_for(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Photo => "webp",
        MediaKind::Video => "jpg",
    }
}

fn encode(image: &RgbImage, kind: MediaKind) -> Result<Vec<u8>> {
    match kind {
        MediaKind::Photo => Ok(WebpEncoder::from_rgb(image.as_raw(), image.width(), image.height())
            .encode(THUMBNAIL_WEBP_QUALITY)
            .to_vec()),
        MediaKind::Video => {
            let mut buffer = Vec::new();
            JpegEncoder::new_with_quality(&mut buffer, THUMBNAIL_JPEG_QUALITY)
                .encode_image(image)
                .map_err(|e| MediaError::Encode(e.to_string()))?;
            Ok(buffer)
        }
    }
}

fn downscale(image: &RgbImage, max_dim: u32) -> RgbImage {
    let (w, h) = image.dimensions();
    if w.max(h) <= max_dim {
        return image.clone();
    }
    image::imageops::resize(
        image,
        if w >= h { max_dim } else { (w * max_dim / h).max(1) },
        if h >= w { max_dim } else { (h * max_dim / w).max(1) },
        FilterType::Triangle,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards_by_key_prefix() {
        let cache = ThumbnailCache::new("C:\\data\\thumbnails");
        let path = cache.path_for("ab12cd34", MediaKind::Video);
        assert!(path.ends_with(Path::new("ab").join("ab12cd34.jpg")));
    }

    #[test]
    fn photo_thumbnails_are_webp_and_video_thumbnails_are_jpeg() {
        let cache = ThumbnailCache::new("C:\\data\\thumbnails");
        assert!(cache.path_for("k", MediaKind::Photo).ends_with("k.webp"));
        assert!(cache.path_for("k", MediaKind::Video).ends_with("k.jpg"));
    }

    #[test]
    fn downscale_preserves_aspect_ratio() {
        let wide = RgbImage::new(2000, 1000);
        let out = downscale(&wide, 512);
        assert_eq!(out.dimensions(), (512, 256));

        let tall = RgbImage::new(1000, 2000);
        assert_eq!(downscale(&tall, 512).dimensions(), (256, 512));
    }

    #[test]
    fn downscale_never_upscales() {
        let small = RgbImage::new(64, 32);
        assert_eq!(downscale(&small, 512).dimensions(), (64, 32));
    }

    #[test]
    fn store_writes_and_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(dir.path());
        let image = RgbImage::new(800, 600);

        let first = cache.store(&image, "deadbeef", MediaKind::Photo).unwrap();
        assert!(first.is_file());
        assert!(cache.exists("deadbeef", MediaKind::Photo));

        // A second call must not rewrite the file.
        let before = std::fs::metadata(&first).unwrap().len();
        let second = cache.store(&image, "deadbeef", MediaKind::Photo).unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::metadata(&second).unwrap().len(), before);

        // No `.part` files should survive a successful write.
        assert!(!first.with_extension("webp.part").exists());
    }

    #[test]
    fn video_poster_frames_stay_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(dir.path());
        let path = cache
            .store(&RgbImage::new(800, 600), "poster", MediaKind::Video)
            .unwrap();
        assert!(path.is_file());
        assert!(path.ends_with("poster.jpg"));
        assert!(!path.with_extension("jpg.part").exists());
    }

    #[test]
    fn clear_empties_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(dir.path());
        cache.store(&RgbImage::new(10, 10), "aa01", MediaKind::Photo).unwrap();
        cache.store(&RgbImage::new(10, 10), "bb02", MediaKind::Photo).unwrap();
        assert!(cache.size_on_disk() > 0);

        assert_eq!(cache.clear().unwrap(), 2);
        assert!(!cache.exists("aa01", MediaKind::Photo));
    }
}
