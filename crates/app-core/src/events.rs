//! Events pushed from the backend to the UI.
//!
//! Progress is *pushed* rather than polled so the media grid and the progress
//! panel stay live during a long import without the frontend hammering the
//! database (§18).

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use teo_database::models::ProcessingProgress;

pub const PROGRESS: &str = "teo://progress";
pub const SHOOT_CHANGED: &str = "teo://shoot-changed";
pub const LIBRARY_CHANGED: &str = "teo://library-changed";
pub const JOB_FAILED: &str = "teo://job-failed";
pub const EXPORT_PROGRESS: &str = "teo://export-progress";
pub const NOTICE: &str = "teo://notice";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    #[serde(flatten)]
    pub progress: ProcessingProgress,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootChanged {
    pub shoot_id: i64,
    /// What changed, so the UI can invalidate only the affected queries.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobFailed {
    pub shoot_id: i64,
    pub kind: String,
    pub file: Option<String>,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgressEvent {
    pub export_id: i64,
    pub shoot_id: i64,
    pub files_done: usize,
    pub files_total: usize,
    pub files_skipped: usize,
    pub bytes_done: u64,
    pub finished: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub level: String,
    pub message: String,
}

/// Emits an event, logging rather than propagating a failure — a UI that has
/// gone away must never abort background work.
pub fn emit<T: Serialize + Clone>(app: &AppHandle, event: &str, payload: T) {
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!(event, error = %e, "could not emit event");
    }
}

pub fn notice(app: &AppHandle, level: &str, message: impl Into<String>) {
    emit(app, NOTICE, Notice { level: level.to_string(), message: message.into() });
}

pub fn shoot_changed(app: &AppHandle, shoot_id: i64, reason: &str) {
    emit(app, SHOOT_CHANGED, ShootChanged { shoot_id, reason: reason.to_string() });
}
