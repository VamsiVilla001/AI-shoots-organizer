//! A custom URI scheme for serving media to the webview.
//!
//! The alternative — widening Tauri's asset-protocol scope to cover arbitrary
//! disk paths — would let any page the webview loads read any file. This
//! handler instead resolves an id through the database first, so the webview
//! can only reach files this application has actually indexed.
//!
//! Three routes:
//!   * `thumb/<media id>` — the cached thumbnail
//!   * `full/<media id>`  — a web-safe rendering of the original, which is also
//!     how HEIC and camera raw become viewable at all
//!   * `video/<media id>` — the original video, with range support so the
//!     player can seek to a detection timestamp (§9)

use std::path::Path;
use std::sync::Arc;

use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeResponder};
use skwad_database::repo::media as media_repo;

use crate::state::AppState;

pub const SCHEME: &str = "skwadmedia";

/// Longest edge for the `full` rendering. Enough to inspect a face crop at
/// 100%, small enough to send over IPC without a stall.
const FULL_MAX_DIM: u32 = 2048;

/// Chunk returned for a video range request that does not specify an end.
const VIDEO_CHUNK: u64 = 2 * 1024 * 1024;

/// The base URL the frontend should prefix onto media paths. Windows serves
/// custom schemes over `http://<scheme>.localhost`; everywhere else the scheme
/// is used directly.
pub fn url_base() -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost")
    } else {
        format!("{SCHEME}://localhost")
    }
}

/// Entry point wired up in `lib.rs`. Work happens on a worker thread so a large
/// file never blocks the UI.
pub fn handle(app: &AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = app.clone();
    std::thread::spawn(move || {
        let response = route(&app, &request);
        responder.respond(response);
    });
}

fn route(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path().trim_start_matches('/').to_string();
    let mut parts = path.splitn(2, '/');
    let kind = parts.next().unwrap_or_default();
    let id: i64 = match parts.next().and_then(|v| v.split('?').next()).and_then(|v| v.parse().ok()) {
        Some(id) => id,
        None => return error(StatusCode::BAD_REQUEST, "expected /<kind>/<id>"),
    };

    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "application is still starting");
    };

    let media = match state.db.conn().and_then(|conn| media_repo::get_by_id(&conn, id)) {
        Ok(Some(media)) => media,
        Ok(None) => return error(StatusCode::NOT_FOUND, "not indexed"),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    match kind {
        "thumb" => serve_thumbnail(&media),
        "full" => serve_full(&state, &media),
        "video" => serve_video(request, &media),
        _ => error(StatusCode::NOT_FOUND, "unknown route"),
    }
}

fn serve_thumbnail(media: &skwad_database::models::Media) -> Response<Vec<u8>> {
    let Some(path) = media.thumbnail_path.as_ref() else {
        return error(StatusCode::NOT_FOUND, "no thumbnail yet");
    };
    match std::fs::read(path) {
        Ok(bytes) => ok(bytes, "image/jpeg", true),
        Err(e) => error(StatusCode::NOT_FOUND, &e.to_string()),
    }
}

/// Renders the original into something a browser can display.
///
/// JPEG and PNG are streamed straight through. Anything else — HEIC, TIFF,
/// camera raw — is decoded and re-encoded, which is what makes those formats
/// previewable in the first place.
fn serve_full(state: &Arc<AppState>, media: &skwad_database::models::Media) -> Response<Vec<u8>> {
    let path = Path::new(&media.path);
    if !path.is_file() {
        return error(StatusCode::NOT_FOUND, "the original file has moved or been deleted");
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
            return ok(bytes, mime, true);
        }
    }

    let ffmpeg = crate::pipeline::discover_ffmpeg(&state.settings());
    let orientation = media.orientation.clamp(1, 8) as u16;
    match skwad_media_core::decode::load_image(path, orientation, Some(FULL_MAX_DIM), ffmpeg.as_ref()) {
        Ok(image) => {
            let mut buffer = Vec::new();
            match image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, 90).encode_image(&image) {
                Ok(()) => ok(buffer, "image/jpeg", true),
                Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        }
        Err(e) => error(StatusCode::UNSUPPORTED_MEDIA_TYPE, &e.to_string()),
    }
}

/// Serves a video, honouring `Range` so the player can seek. Without range
/// support the `<video>` element refuses to scrub, which would break jumping
/// to a detection timestamp.
fn serve_video(request: &Request<Vec<u8>>, media: &skwad_database::models::Media) -> Response<Vec<u8>> {
    let path = Path::new(&media.path);
    let Ok(metadata) = std::fs::metadata(path) else {
        return error(StatusCode::NOT_FOUND, "the original file has moved or been deleted");
    };
    let total = metadata.len();
    let mime = video_mime(&media.extension);

    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_range(v, total));

    let Some((start, end)) = range else {
        return match std::fs::read(path) {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .body(bytes)
                .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")),
            Err(e) => error(StatusCode::NOT_FOUND, &e.to_string()),
        };
    };

    match read_range(path, start, end) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, mime)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(header::CONTENT_LENGTH, bytes.len().to_string())
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(bytes)
            .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
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
pub(crate) fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
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

fn video_mime(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

fn ok(bytes: Vec<u8>, mime: &str, cacheable: bool) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");

    if cacheable {
        // Thumbnails are content-addressed and full renders are derived from an
        // immutable original, so both are safe to cache for the session.
        builder = builder.header(header::CACHE_CONTROL, "max-age=3600");
    }

    builder
        .body(bytes)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

fn error(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(message.as_bytes().to_vec())
        .expect("static error response is always valid")
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
    fn the_url_base_matches_the_platform_convention() {
        let base = url_base();
        assert!(base.contains(SCHEME));
        if cfg!(windows) {
            assert!(base.starts_with("http://"));
        }
    }
}
