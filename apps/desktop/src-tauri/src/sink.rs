//! The desktop front door's implementation of the core's event sink.

use tauri::{AppHandle, Emitter};
use teo_app_core::ProgressSink;

/// Forwards core events to the webview as Tauri events, under the same names
/// the React side already listens for (`teo://progress` and friends).
pub struct TauriProgressSink {
    app: AppHandle,
}

impl TauriProgressSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ProgressSink for TauriProgressSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        // A window that has gone away must never abort background work, so a
        // failed emit is a log line and nothing more.
        if let Err(e) = self.app.emit(event, payload) {
            tracing::debug!(event, error = %e, "could not emit event to the webview");
        }
    }
}
