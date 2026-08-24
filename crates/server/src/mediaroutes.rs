//! `/media/:id/{thumb,full,stream}` — the HTTP replacement for `teomedia://`.
//!
//! Ids in, bytes out. No route here accepts a path, so a browser can only reach
//! files this application has indexed — the same property the custom protocol
//! gives the desktop webview, and the reason both call into
//! [`teo_app_core::media`] rather than reimplementing the lookup.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use teo_app_core::media;

use crate::error::{blocking, ApiResult};
use crate::state::ServerState;

/// Derived from an immutable original, so safe to cache for the session.
const CACHE: &str = "max-age=3600";

pub async fn thumbnail(
    State(state): State<Arc<ServerState>>,
    Path(media_id): Path<i64>,
) -> ApiResult<Response> {
    let core = Arc::clone(&state.core);
    let payload = blocking(move || {
        let item = media::lookup(&core.db, media_id)?;
        Ok(media::thumbnail(&item)?)
    })
    .await?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, payload.mime), (header::CACHE_CONTROL, CACHE)],
        payload.bytes,
    )
        .into_response())
}

#[derive(Debug, serde::Deserialize)]
pub struct FrameQuery {
    /// Seconds into the clip. Omitted means the first frame.
    pub t: Option<f64>,
}

/// One frame of a video, for cropping a face that was detected part-way in.
///
/// Falls back to the cached poster frame if the render fails: a missing FFmpeg
/// or a file that has moved should leave a tile showing the wrong moment, which
/// is what it showed before this route existed, rather than a broken image.
pub async fn frame(
    State(state): State<Arc<ServerState>>,
    Path(media_id): Path<i64>,
    Query(query): Query<FrameQuery>,
) -> ApiResult<Response> {
    let core = Arc::clone(&state.core);
    let at = query.t.unwrap_or(0.0);
    let payload = blocking(move || {
        let item = media::lookup(&core.db, media_id)?;
        match media::frame(&core.settings(), &item, at) {
            Ok(payload) => Ok(payload),
            Err(error) => {
                tracing::debug!(media_id, at, %error, "frame render failed; serving the poster frame");
                Ok(media::thumbnail(&item)?)
            }
        }
    })
    .await?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, payload.mime), (header::CACHE_CONTROL, CACHE)],
        payload.bytes,
    )
        .into_response())
}

/// A web-safe rendering of the original — also the only way HEIC and camera raw
/// become viewable at all.
pub async fn full(State(state): State<Arc<ServerState>>, Path(media_id): Path<i64>) -> ApiResult<Response> {
    let core = Arc::clone(&state.core);
    let payload = blocking(move || {
        let item = media::lookup(&core.db, media_id)?;
        Ok(media::full_render(&core.settings(), &item)?)
    })
    .await?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, payload.mime), (header::CACHE_CONTROL, CACHE)],
        payload.bytes,
    )
        .into_response())
}

/// The original video, honouring `Range` so a player can seek straight to a
/// detection timestamp.
pub async fn stream(
    State(state): State<Arc<ServerState>>,
    Path(media_id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    let core = Arc::clone(&state.core);
    let slice = blocking(move || {
        let item = media::lookup(&core.db, media_id)?;
        Ok(media::video_slice(&item, range.as_deref())?)
    })
    .await?;

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, slice.mime)
        .header(header::ACCEPT_RANGES, "bytes");

    builder = match slice.range {
        Some((start, end)) => builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{}", slice.total)),
        None => builder.status(StatusCode::OK),
    };

    builder
        .body(axum::body::Body::from(slice.bytes))
        .map_err(|e| crate::error::ApiError::internal(e.to_string()))
}
