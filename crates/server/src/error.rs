//! One error shape for the whole HTTP surface.
//!
//! The Tauri bridge returns `{ "message": … }` and the UI shows it verbatim,
//! so the HTTP layer answers the same way: a status code for machines and one
//! actionable sentence for the person reading it.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use skwad_app_core::api::ErrorKind;
use skwad_app_core::media::MediaError;

use crate::config::JailError;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
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

impl From<skwad_app_core::api::ApiError> for ApiError {
    fn from(e: skwad_app_core::api::ApiError) -> Self {
        let status = match e.kind {
            ErrorKind::BadRequest => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, e.message)
    }
}

/// Media errors already carry the right distinction between "gone" and
/// "cannot be rendered"; keep it.
impl From<MediaError> for ApiError {
    fn from(e: MediaError) -> Self {
        let status = match &e {
            MediaError::NotIndexed | MediaError::NotFound(_) => StatusCode::NOT_FOUND,
            MediaError::Unsupported(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            MediaError::BadRequest(_) => StatusCode::BAD_REQUEST,
            MediaError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, e.to_string())
    }
}

impl From<JailError> for ApiError {
    fn from(e: JailError) -> Self {
        match e {
            JailError::NoRoots => Self::bad_request(e.to_string()),
            JailError::Unreadable(_) => Self::not_found(e.to_string()),
            // The same message whether the path exists or not: a 403 that
            // distinguishes them is a probe for what is on the disk.
            JailError::Outside(_) => Self::forbidden(e.to_string()),
        }
    }
}

impl From<skwad_database::DbError> for ApiError {
    fn from(e: skwad_database::DbError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e.to_string())
    }
}

/// Runs blocking work — every database call, every filesystem walk, every
/// command — off the async runtime. The core is synchronous by design, so
/// handlers must not hold a runtime thread while it works.
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
    fn core_error_kinds_map_onto_statuses() {
        let forbidden: ApiError = skwad_app_core::api::ApiError::forbidden("no").into();
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
        let bad: ApiError = skwad_app_core::api::ApiError::bad_request("no").into();
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn media_errors_keep_their_distinctions() {
        let gone: ApiError = MediaError::NotIndexed.into();
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
