//! `.skwad` catalogues crossing the network: `/api/catalogues/*`.
//!
//! A client has no file dialog into the server's disk. So a catalogue it
//! wants to load is uploaded here first — into `<library>/catalogues/inbox`
//! — and then loaded by path with the ordinary `load_skwad` command; one it
//! published (to `<library>/catalogues/outbox`, the default when a client
//! gives no destination) is fetched from here. Both folders are the only
//! places these routes will touch, so a path in a query string cannot reach
//! anything else on the box.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, Request, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tower::ServiceExt;

use crate::config::resolve_within_roots;
use crate::error::{blocking, ApiError, ApiResult};
use crate::state::ServerState;

pub const INBOX: &str = "catalogues/inbox";
pub const OUTBOX: &str = "catalogues/outbox";

/// Uploads are bounded well under the core's own 512 MiB package limit.
pub const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// The file's own name, for the inbox entry. Only its basename is used.
    pub name: Option<String>,
}

fn safe_basename(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "catalogue.skwad".to_string()
    } else if trimmed.to_ascii_lowercase().ends_with(".skwad") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.skwad")
    }
}

/// `POST /api/catalogues/upload?name=` — the bytes of a `.skwad` a client
/// wants to load. Answers with the server path to pass to `load_skwad`.
pub async fn upload(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    if body.is_empty() {
        return Err(ApiError::bad_request("the upload is empty"));
    }
    if body.len() > MAX_UPLOAD_BYTES {
        return Err(ApiError::bad_request("SKWAD packages are limited to 512 MiB"));
    }
    let inbox = state.core.paths.root.join(INBOX);
    let name = safe_basename(query.name.as_deref().unwrap_or("catalogue.skwad"));
    blocking(move || {
        std::fs::create_dir_all(&inbox)?;
        let target = inbox.join(format!("{}-{name}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&target, &body)?;
        Ok(Json(serde_json::json!({ "path": target.to_string_lossy() })))
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct DownloadQuery {
    /// The server path `publish_skwad` answered with.
    pub path: String,
}

/// `GET /api/catalogues/download?path=` — a published catalogue, for the
/// person who asked for it. Only files under the outbox are served.
pub async fn download(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<DownloadQuery>,
    request: Request,
) -> ApiResult<Response> {
    let outbox = state.core.paths.root.join(OUTBOX);
    let requested = PathBuf::from(&query.path);
    let resolved = blocking(move || {
        let roots = [outbox];
        Ok(resolve_within_roots(&requested, &roots)?)
    })
    .await?;
    if !resolved.is_file() {
        return Err(ApiError::not_found("no such published catalogue"));
    }
    let file_name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "catalogue.skwad".into());
    let served = match tower_http::services::ServeFile::new(&resolved).oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    let mut response = served.into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\"")) {
        response.headers_mut().insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_are_reduced_to_a_safe_basename() {
        assert_eq!(safe_basename("C:\\Users\\x\\match day.skwad"), "match day.skwad");
        assert_eq!(safe_basename("../../etc/passwd"), "passwd.skwad");
        assert_eq!(safe_basename(""), "catalogue.skwad");
        assert_eq!(safe_basename("day.SKWAD"), "day.SKWAD");
    }
}
