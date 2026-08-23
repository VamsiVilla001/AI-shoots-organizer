//! Serving indexed media, independent of transport.
//!
//! Both front doors resolve a database id and hand back bytes: the desktop over
//! the `teomedia://` scheme, the server over HTTP. Neither ever takes a path
//! from the caller — that is the property that stops a webview or a browser
//! from reading arbitrary disk — so the id lookup, the HEIC/raw render and the
//! `Range` arithmetic all live here rather than in either adapter.

use std::path::Path;

use teo_database::models::Media;
use teo_database::repo::media as media_repo;
use teo_database::Database;

use crate::pipeline;
use crate::settings::AppSettings;

/// Longest edge for the `full` rendering. Enough to inspect a face crop at
/// 100%, small enough to send without a stall.
pub const FULL_MAX_DIM: u32 = 2048;

/// Chunk returned for a video range request that does not specify an end.
pub const VIDEO_CHUNK: u64 = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("not indexed")]
    NotIndexed,
    #[error("no thumbnail yet")]
    NoThumbnail,
    #[error("the original file has moved or been deleted")]
    Missing,
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Io(String),
    #[error(transparent)]
    Database(#[from] teo_database::DbError),
}

/// Bytes plus the content type they should be served as.
#[derive(Debug, Clone)]
pub struct Payload {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
}

/// One slice of a video, and what the whole file measures.
#[derive(Debug, Clone)]
pub struct VideoSlice {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub total: u64,
    /// `None` when the whole file is being returned.
    pub range: Option<(u64, u64)>,
}

/// The id lookup every route starts with.
pub fn lookup(db: &Database, media_id: i64) -> Result<Media, MediaError> {
    let conn = db.conn()?;
    media_repo::get_by_id(&conn, media_id)?.ok_or(MediaError::NotIndexed)
}

pub fn thumbnail(media: &Media) -> Result<Payload, MediaError> {
    let path = media.thumbnail_path.as_ref().ok_or(MediaError::NoThumbnail)?;
    let bytes = std::fs::read(path).map_err(|e| MediaError::Io(e.to_string()))?;
    Ok(Payload { bytes, mime: "image/jpeg" })
}

/// Renders the original into something a browser can display.
///
/// JPEG, PNG and WebP are streamed straight through — the browser applies EXIF
/// orientation itself. Anything else (HEIC, TIFF, camera raw) is decoded and
/// re-encoded, which is what makes those formats previewable at all.
pub fn full_render(settings: &AppSettings, media: &Media) -> Result<Payload, MediaError> {
    let path = Path::new(&media.path);
    if !path.is_file() {
        return Err(MediaError::Missing);
    }

    let extension = media.extension.to_ascii_lowercase();
    if matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        if let Ok(bytes) = std::fs::read(path) {
            let mime = match extension.as_str() {
                "png" => "image/png",
                "webp" => "image/webp",
                _ => "image/jpeg",
            };
            return Ok(Payload { bytes, mime });
        }
    }

    let ffmpeg = pipeline::discover_ffmpeg(settings);
    let orientation = media.orientation.clamp(1, 8) as u16;
    let image = teo_media_core::decode::load_image(path, orientation, Some(FULL_MAX_DIM), ffmpeg.as_ref())
        .map_err(|e| MediaError::Unsupported(e.to_string()))?;

    let mut buffer = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, 90)
        .encode_image(&image)
        .map_err(|e| MediaError::Io(e.to_string()))?;
    Ok(Payload { bytes: buffer, mime: "image/jpeg" })
}

/// Reads a video, honouring a `Range` header value when one is supplied.
///
/// Range support is what lets a player scrub to a detection timestamp; without
/// it the `<video>` element refuses to seek at all.
pub fn video_slice(media: &Media, range_header: Option<&str>) -> Result<VideoSlice, MediaError> {
    let path = Path::new(&media.path);
    let metadata = std::fs::metadata(path).map_err(|_| MediaError::Missing)?;
    let total = metadata.len();
    let mime = video_mime(&media.extension);

    match range_header.and_then(|value| parse_range(value, total)) {
        Some((start, end)) => {
            let bytes = read_range(path, start, end).map_err(|e| MediaError::Io(e.to_string()))?;
            Ok(VideoSlice { bytes, mime, total, range: Some((start, end)) })
        }
        None => {
            let bytes = std::fs::read(path).map_err(|e| MediaError::Io(e.to_string()))?;
            Ok(VideoSlice { bytes, mime, total, range: None })
        }
    }
}

pub fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
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
    fn read_range_returns_exactly_the_slice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        assert_eq!(read_range(&path, 0, 3).unwrap(), b"0123");
        assert_eq!(read_range(&path, 6, 9).unwrap(), b"6789");
    }

    #[test]
    fn a_missing_original_is_reported_not_panicked() {
        let media = Media {
            id: 1,
            shoot_id: 1,
            path: "Z:\\gone\\clip.mp4".into(),
            filename: "clip.mp4".into(),
            media_type: "video".into(),
            extension: "mp4".into(),
            width: None,
            height: None,
            duration: None,
            file_size: 0,
            content_key: "k".into(),
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
            processing_status: "analysed".into(),
            face_count: 0,
            person_count: 0,
            error: None,
        };

        assert!(matches!(video_slice(&media, None), Err(MediaError::Missing)));
        assert!(matches!(thumbnail(&media), Err(MediaError::NoThumbnail)));
    }
}
