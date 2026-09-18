//! A custom URI scheme for serving media to the webview.
//!
//! The alternative — widening Tauri's asset-protocol scope to cover arbitrary
//! disk paths — would let any page the webview loads read any file. This
//! handler instead resolves an id through the database first, so the webview
//! can only reach files this application has actually indexed.
//!
//! Four routes, all answered by [`skwad_app_core::media`] — the HTTP server's
//! `/media/…` routes call the same functions, so both front doors serve the
//! same bytes:
//!   * `thumb/<media id>` — the cached thumbnail
//!   * `full/<media id>`  — a web-safe rendering of the original, which is also
//!     how HEIC and camera raw become viewable at all
//!   * `frame/<media id>?t=<seconds>` — one analysed video sample frame, used
//!     by the reviewer to click and name the faces detected at that timestamp
//!   * `video/<media id>` — the original video, with range support so the
//!     player can seek to a detection timestamp (§9)

use std::sync::Arc;

use skwad_app_core::media::{self, MediaError};
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeResponder};

use crate::state::AppState;

pub const SCHEME: &str = "skwadmedia";

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
    let id: i64 = match parts
        .next()
        .and_then(|v| v.split('?').next())
        .and_then(|v| v.parse().ok())
    {
        Some(id) => id,
        None => return error(StatusCode::BAD_REQUEST, "expected /<kind>/<id>"),
    };

    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "application is still starting");
    };

    let media_row = match media::lookup(&state.db, id) {
        Ok(row) => row,
        Err(e) => return failure(e),
    };

    match kind {
        "thumb" => payload(media::thumbnail(&media_row)),
        "full" => payload(media::full_render(&state, &media_row)),
        "frame" => {
            let Some(timestamp) = request.uri().query().and_then(media::parse_frame_timestamp) else {
                return error(StatusCode::BAD_REQUEST, "expected a finite t=<seconds> query");
            };
            payload(media::frame(&state, &media_row, timestamp))
        }
        "video" => {
            let range = request
                .headers()
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok());
            match media::video_slice(&media_row, range) {
                Ok(slice) => video(slice),
                Err(e) => failure(e),
            }
        }
        _ => error(StatusCode::NOT_FOUND, "unknown route"),
    }
}

fn payload(result: media::Result<media::Payload>) -> Response<Vec<u8>> {
    match result {
        Ok(p) => ok(p.bytes, p.mime, p.cacheable),
        Err(e) => failure(e),
    }
}

fn video(slice: media::VideoSlice) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, slice.mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    builder = match slice.range {
        Some((start, end)) => builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{}", slice.total))
            .header(header::CONTENT_LENGTH, slice.bytes.len().to_string()),
        None => builder.status(StatusCode::OK),
    };
    builder
        .body(slice.bytes)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

fn failure(e: MediaError) -> Response<Vec<u8>> {
    let status = match &e {
        MediaError::NotIndexed | MediaError::NotFound(_) => StatusCode::NOT_FOUND,
        MediaError::Unsupported(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        MediaError::BadRequest(_) => StatusCode::BAD_REQUEST,
        MediaError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, &e.to_string())
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
    fn the_url_base_matches_the_platform_convention() {
        let base = url_base();
        assert!(base.contains(SCHEME));
        if cfg!(windows) {
            assert!(base.starts_with("http://"));
        }
    }
}
