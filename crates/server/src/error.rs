//! One error shape for the whole API.
//!
//! The Tauri layer returns `{ "message": … }` and the UI shows it verbatim, so
//! the HTTP layer answers the same way: a status code for machines and one
//! actionable sentence for the person reading it.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use teo_app_core::media::MediaError;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    /// A request that cannot be satisfied as written — the equivalent of the
    /// command layer's `err(...)`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Anything at 500 is ours to fix, so it goes to the log as well as to
        // the caller; client errors are the caller's business only.
        if self.status.is_server_error() {
            tracing::error!(status = %self.status, message = %self.message, "request failed");
        }
        (self.status, Json(serde_json::json!({ "message": self.message }))).into_response()
    }
}

/// A database failure is not the caller's fault, so it reads as a server error
/// while still carrying the detail the desktop app would have shown.
impl From<teo_database::DbError> for ApiError {
    fn from(e: teo_database::DbError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<teo_export_engine::ExportError> for ApiError {
    fn from(e: teo_export_engine::ExportError) -> Self {
        match e {
            // "destination inside the source" is a rejected request, not a bug.
            teo_export_engine::ExportError::DestinationInsideSource
            | teo_export_engine::ExportError::Destination(_) => Self::bad_request(e.to_string()),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<teo_app_core::export::ExportRunError> for ApiError {
    fn from(e: teo_app_core::export::ExportRunError) -> Self {
        use teo_app_core::export::ExportRunError;
        match e {
            ExportRunError::Engine(engine) => engine.into(),
            ExportRunError::Other(message) => Self::bad_request(message),
            ExportRunError::Database(db) => db.into(),
        }
    }
}

/// A stage failing is an operational problem — a missing model, a folder that
/// vanished — rather than a malformed request.
impl From<teo_app_core::stages::StageError> for ApiError {
    fn from(e: teo_app_core::stages::StageError) -> Self {
        Self::internal(e.to_string())
    }
}

/// Media errors already carry the right distinction between "gone" and
/// "cannot be rendered"; keep it.
impl From<MediaError> for ApiError {
    fn from(e: MediaError) -> Self {
        match e {
            MediaError::NotIndexed | MediaError::NoThumbnail | MediaError::Missing => {
                Self::not_found(e.to_string())
            }
            MediaError::Unsupported(_) => Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, e.to_string()),
            MediaError::Io(_) | MediaError::Database(_) => Self::internal(e.to_string()),
        }
    }
}

impl From<teo_media_core::MediaError> for ApiError {
    fn from(e: teo_media_core::MediaError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e.to_string())
    }
}

/// Runs blocking work — every database call, every filesystem walk — off the
/// async runtime. The core is synchronous by design (pooled SQLite, rayon,
/// std threads), so handlers must not hold a runtime thread while it works.
pub async fn blocking<T, F>(f: F) -> ApiResult<T>
where
    F: FnOnce() -> ApiResult<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(ApiError::internal(format!("a worker task failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_destination_is_a_client_error_not_a_crash() {
        let error: ApiError = teo_export_engine::ExportError::DestinationInsideSource.into();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("source folder"), "got {}", error.message);
    }

    #[test]
    fn media_errors_keep_their_distinctions() {
        let gone: ApiError = MediaError::Missing.into();
        assert_eq!(gone.status, StatusCode::NOT_FOUND);

        let undecodable: ApiError = MediaError::Unsupported("no decoder".into()).into();
        assert_eq!(undecodable.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn blocking_propagates_the_inner_error() {
        let result: ApiResult<()> = blocking(|| Err(ApiError::bad_request("no"))).await;
        assert_eq!(result.unwrap_err().status, StatusCode::BAD_REQUEST);
    }
}
