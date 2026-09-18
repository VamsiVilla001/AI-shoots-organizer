//! `/media/{kind}/{id}` — the HTTP replacement for `skwadmedia://`.
//!
//! The same four routes, at the same paths, so the frontend's URL builder only
//! changes its base. Thumbnails and frames are content-addressed, so they
//! carry an `ETag` of the content key and an immutable cache policy: a
//! 500-image grid costs 500 requests the first time and none the second.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use skwad_app_core::media;

use crate::error::{blocking, ApiError, ApiResult};
use crate::state::ServerState;

/// Safe because the key changes whenever the file does.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

pub async fn serve(
    State(state): State<Arc<ServerState>>,
    Path((kind, media_id)): Path<(String, i64)>,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> ApiResult<Response> {
    let core = Arc::clone(&state.core);
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match kind.as_str() {
        "thumb" | "frame" | "full" => {
            let timestamp = if kind == "frame" {
                Some(
                    query
                        .as_deref()
                        .and_then(media::parse_frame_timestamp)
                        .ok_or_else(|| ApiError::bad_request("expected a finite t=<seconds> query"))?,
                )
            } else {
                None
            };
            let etag_key = format!("{}:{}:{}", kind, media_id, query.as_deref().unwrap_or_default());
            let (payload, etag) = blocking(move || {
                let row = media::lookup(&core.db, media_id)?;
                let etag = format!("\"{}\"", blake3::hash(format!("{}|{etag_key}", row.content_key).as_bytes()).to_hex());
                if if_none_match.as_deref() == Some(etag.as_str()) {
                    return Ok((None, etag));
                }
                let payload = match kind.as_str() {
                    "thumb" => media::thumbnail(&row)?,
                    "frame" => media::frame(&core, &row, timestamp.unwrap_or(0.0))?,
                    _ => media::full_render(&core, &row)?,
                };
                Ok((Some(payload), etag))
            })
            .await?;

            let Some(payload) = payload else {
                return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
            };
            Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, payload.mime.to_string()),
                    (header::ETAG, etag),
                    (
                        header::CACHE_CONTROL,
                        if payload.cacheable { IMMUTABLE } else { "no-store" }.to_string(),
                    ),
                ],
                payload.bytes,
            )
                .into_response())
        }
        "video" => {
            let slice = blocking(move || {
                let row = media::lookup(&core.db, media_id)?;
                Ok(media::video_slice(&row, range.as_deref())?)
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
                .map_err(|e| ApiError::internal(e.to_string()))
        }
        _ => Err(ApiError::not_found("unknown media route")),
    }
}
