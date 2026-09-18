//! Media bytes for a front end: thumbnails, full renders, video frames and
//! ranged video.
//!
//! Ids in, bytes out. Nothing here accepts a path from a caller, so a front
//! end can only reach files this application has indexed — the property the
//! desktop's `skwadmedia://` scheme exists to give, and the reason the HTTP
//! server's media routes call into here rather than reimplementing the
//! lookup. Each front door only turns a [`MediaError`] into its own status
//! code.

use std::path::Path;

use skwad_database::models::{Media, MediaType};
use skwad_database::repo::media as media_repo;
use skwad_database::Database;

use crate::state::AppState;

/// Longest edge for the `full` rendering. Enough to inspect a face crop at
/// 100%, small enough to send over IPC without a stall.
pub const FULL_MAX_DIM: u32 = 2048;

/// Matches the video analysis working size, keeping face boxes pixel-for-pixel
/// consistent while avoiding a fresh 4K image in the webview.
pub const VIDEO_FRAME_MAX_DIM: u32 = 1280;

/// Chunk returned for a video range request that does not specify an end.
pub const VIDEO_CHUNK: u64 = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("not indexed")]
    NotIndexed,
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, MediaError>;

impl From<skwad_database::DbError> for MediaError {
    fn from(e: skwad_database::DbError) -> Self {
        MediaError::Internal(e.to_string())
    }
}

/// Bytes plus what they are.
#[derive(Debug, Clone)]
pub struct Payload {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    /// Derived from an immutable original (or content-addressed), so a front
    /// door may tell the client to cache it.
    pub cacheable: bool,
}

/// A slice of a video file, for a ranged response.
#[derive(Debug, Clone)]
pub struct VideoSlice {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub total: u64,
    /// `Some((start, end))` inclusive for a partial response, `None` for the
    /// whole file.
    pub range: Option<(u64, u64)>,
}

pub fn lookup(db: &Database, media_id: i64) -> Result<Media> {
    let mut conn = db.conn()?;
    media_repo::get_by_id(&mut conn, media_id)?.ok_or(MediaError::NotIndexed)
}

/// The cached thumbnail. Photo thumbnails are WebP, video posters JPEG.
pub fn thumbnail(media: &Media) -> Result<Payload> {
    let Some(path) = media.thumbnail_path.as_ref() else {
        return Err(MediaError::NotFound("no thumbnail yet".into()));
    };
    let mime = if Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("webp"))
    {
        "image/webp"
    } else {
        "image/jpeg"
    };
    let bytes = std::fs::read(path).map_err(|e| MediaError::NotFound(e.to_string()))?;
    Ok(Payload {
        bytes,
        mime,
        cacheable: true,
    })
}

/// Renders the original into something a browser can display.
///
/// JPEG and PNG are streamed straight through. Anything else — HEIC, TIFF,
/// camera raw — is decoded and re-encoded, which is what makes those formats
/// previewable in the first place.
pub fn full_render(state: &AppState, media: &Media) -> Result<Payload> {
    let path = Path::new(&media.path);
    if !path.is_file() {
        return Err(MediaError::NotFound(
            "the original file has moved or been deleted".into(),
        ));
    }

    let extension = media.extension.to_ascii_lowercase();
    if matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        // The browser handles EXIF orientation for these itself.
        if let Ok(bytes) = std::fs::read(path) {
            let mime = match extension.as_str() {
                "png" => "image/png",
                "webp" => "image/webp",
                _ => "image/jpeg",
            };
            return Ok(Payload {
                bytes,
                mime,
                cacheable: true,
            });
        }
    }

    let ffmpeg = crate::pipeline::discover_ffmpeg(&state.settings());
    let orientation = media.orientation.clamp(1, 8) as u16;
    let image = skwad_media_core::decode::load_image(path, orientation, Some(FULL_MAX_DIM), ffmpeg.as_ref())
        .map_err(|e| MediaError::Unsupported(e.to_string()))?;
    let mut buffer = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, 90)
        .encode_image(&image)
        .map_err(|e| MediaError::Internal(e.to_string()))?;
    Ok(Payload {
        bytes: buffer,
        mime: "image/jpeg",
        cacheable: true,
    })
}

/// One of the frames used during video analysis. New analysis jobs cache
/// face-bearing samples while their pixels are already in memory. Older items
/// are decoded once (from the lightweight proxy when possible) and cached.
pub fn frame(state: &AppState, media: &Media, timestamp: f64) -> Result<Payload> {
    if media.media_type != MediaType::Video.as_str() {
        return Err(MediaError::BadRequest(
            "sample frames are only available for videos".into(),
        ));
    }
    if !timestamp.is_finite() || timestamp < 0.0 {
        return Err(MediaError::BadRequest("expected a finite t=<seconds> query".into()));
    }
    match state.video_frames.read(&media.content_key, timestamp) {
        Ok(Some(bytes)) => {
            return Ok(Payload {
                bytes,
                mime: "image/jpeg",
                cacheable: true,
            })
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(media = media.id, at = timestamp, %error, "could not read cached review frame"),
    }
    let path = Path::new(&media.path);
    if !path.is_file() {
        return Err(MediaError::NotFound(
            "the original file has moved or been deleted".into(),
        ));
    }
    let Some(ffmpeg) = crate::pipeline::discover_ffmpeg(&state.settings()) else {
        return Err(MediaError::Unsupported(
            "FFmpeg is required for video sample frames".into(),
        ));
    };
    let proxy = state.proxies.path_for(&media.content_key);
    let (decode_path, orientation) = if proxy.is_file() {
        (proxy.as_path(), 1)
    } else {
        (path, media.orientation.clamp(1, 8) as u16)
    };
    let image = skwad_media_core::decode::load_video_frame(
        decode_path,
        timestamp,
        orientation,
        Some(VIDEO_FRAME_MAX_DIM),
        &ffmpeg,
    )
    .map_err(|e| MediaError::Unsupported(e.to_string()))?;
    let bytes = state
        .video_frames
        .store(&image, &media.content_key, timestamp)
        .map_err(|e| MediaError::Internal(e.to_string()))?;
    Ok(Payload {
        bytes,
        mime: "image/jpeg",
        cacheable: true,
    })
}

/// The original video, honouring a `Range` header so the player can seek.
/// Without range support the `<video>` element refuses to scrub, which would
/// break jumping to a detection timestamp.
pub fn video_slice(media: &Media, range_header: Option<&str>) -> Result<VideoSlice> {
    let path = Path::new(&media.path);
    let mime = video_mime(&media.extension);
    let metadata = std::fs::metadata(path).map_err(|_| {
        MediaError::NotFound("the original file has moved or been deleted".into())
    })?;
    let total = metadata.len();

    let Some((start, end)) = range_header.and_then(|value| parse_range(value, total)) else {
        let bytes = std::fs::read(path).map_err(|e| MediaError::NotFound(e.to_string()))?;
        return Ok(VideoSlice {
            bytes,
            mime,
            total,
            range: None,
        });
    };

    let bytes = read_range(path, start, end).map_err(|e| MediaError::Internal(e.to_string()))?;
    Ok(VideoSlice {
        bytes,
        mime,
        total,
        range: Some((start, end)),
    })
}

fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let length = (end - start + 1) as usize;
    let mut buffer = vec![0u8; length];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Parses a single-range `bytes=start-end` header. Multi-range requests are
/// deliberately unsupported; no browser sends them for `<video>`.
pub fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = value.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start_text, end_text) = spec.split_once('-')?;

    let (start, end) = if start_text.is_empty() {
        // `bytes=-500` means the *last* 500 bytes.
        let length: u64 = end_text.parse().ok()?;
        if length == 0 {
            return None;
        }
        (total.saturating_sub(length), total - 1)
    } else {
        let start: u64 = start_text.parse().ok()?;
        let end = if end_text.is_empty() {
            (start + VIDEO_CHUNK - 1).min(total - 1)
        } else {
            end_text.parse::<u64>().ok()?.min(total - 1)
        };
        (start, end)
    };

    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

pub fn video_mime(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

/// The `t=<seconds>` parameter of a frame request, if it is usable.
pub fn parse_frame_timestamp(query: &str) -> Option<f64> {
    let value = query.split('&').find_map(|part| {
        part.split_once('=')
            .filter(|(key, _)| *key == "t")
            .map(|(_, value)| value)
    })?;
    let timestamp = value.parse::<f64>().ok()?;
    (timestamp.is_finite() && timestamp >= 0.0).then_some(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bounded_range() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-999", 1000), Some((500, 999)));
    }

    #[test]
    fn an_open_ended_range_is_capped_to_a_chunk() {
        let (start, end) = parse_range("bytes=0-", 100_000_000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, VIDEO_CHUNK - 1);
    }

    #[test]
    fn an_open_ended_range_never_runs_past_the_file() {
        assert_eq!(parse_range("bytes=0-", 500), Some((0, 499)));
    }

    #[test]
    fn a_suffix_range_reads_from_the_end() {
        assert_eq!(parse_range("bytes=-500", 1000), Some((500, 999)));
        // A suffix longer than the file clamps to the whole file.
        assert_eq!(parse_range("bytes=-5000", 1000), Some((0, 999)));
    }

    #[test]
    fn an_end_past_the_file_is_clamped() {
        assert_eq!(parse_range("bytes=900-99999", 1000), Some((900, 999)));
    }

    #[test]
    fn malformed_and_impossible_ranges_are_rejected() {
        assert_eq!(parse_range("items=0-10", 1000), None);
        assert_eq!(parse_range("bytes=abc-def", 1000), None);
        assert_eq!(parse_range("bytes=900-100", 1000), None, "start after end");
        assert_eq!(parse_range("bytes=5000-6000", 1000), None, "start past the file");
        assert_eq!(parse_range("bytes=0-99", 0), None, "empty file");
    }

    #[test]
    fn video_mime_types_cover_the_supported_formats() {
        assert_eq!(video_mime("MP4"), "video/mp4");
        assert_eq!(video_mime("mov"), "video/quicktime");
        assert_eq!(video_mime("mkv"), "video/x-matroska");
        assert_eq!(video_mime("xyz"), "application/octet-stream");
    }

    #[test]
    fn sample_frame_timestamp_is_finite_and_non_negative() {
        assert_eq!(parse_frame_timestamp("t=12.500"), Some(12.5));
        assert_eq!(parse_frame_timestamp("mode=review&t=0"), Some(0.0));
        assert_eq!(parse_frame_timestamp("t=-1"), None);
        assert_eq!(parse_frame_timestamp("t=NaN"), None);
        assert_eq!(parse_frame_timestamp("x=1"), None);
    }

    #[test]
    fn a_video_slice_honours_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, (0u8..=99).collect::<Vec<_>>()).unwrap();
        let media = Media {
            id: 1,
            shoot_id: 1,
            path: path.display().to_string(),
            filename: "clip.mp4".into(),
            media_type: "video".into(),
            extension: "mp4".into(),
            width: None,
            height: None,
            duration: None,
            file_size: 100,
            content_key: "k".into(),
            normalized_relative_path: None,
            captured_at: None,
            indexed_at: "now".into(),
            camera_make: None,
            camera_model: None,
            lens: None,
            iso: None,
            focal_length: None,
            aperture: None,
            shutter: None,
            orientation: 1,
            thumbnail_path: None,
            processing_status: "pending".into(),
            face_count: 0,
            person_count: 0,
            quality_score: None,
            sharpness_score: None,
            exposure_score: None,
            perceptual_hash: None,
            duplicate_group_id: None,
            duplicate_count: 1,
            is_best_shot: false,
            rating: 0,
            pick_state: "none".into(),
            error: None,
        };
        let whole = video_slice(&media, None).unwrap();
        assert_eq!(whole.bytes.len(), 100);
        assert_eq!(whole.range, None);
        assert_eq!(whole.mime, "video/mp4");

        let part = video_slice(&media, Some("bytes=10-19")).unwrap();
        assert_eq!(part.range, Some((10, 19)));
        assert_eq!(part.bytes, (10u8..=19).collect::<Vec<_>>());
        assert_eq!(part.total, 100);
    }
}
