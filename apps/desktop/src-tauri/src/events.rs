//! The Tauri end of the core's [`ProgressSink`] seam.
//!
//! The core pushes events into a sink; here that sink is the Tauri app
//! handle, which fans them out to the webview as `skwad://…` events. The
//! helpers below keep the `events::emit(&app, …)` shape every command already
//! uses, so the commands did not have to learn the seam exists.

use std::sync::Arc;

use serde::Serialize;
use skwad_app_core::ProgressSink;
use tauri::{AppHandle, Emitter};

pub use skwad_app_core::events::*;

/// An [`AppHandle`] as a [`ProgressSink`].
#[derive(Clone)]
pub struct TauriSink(pub AppHandle);

impl ProgressSink for TauriSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Err(e) = self.0.emit(event, payload) {
            tracing::debug!(event, error = %e, "could not emit event");
        }
    }
}

/// The sink the core's workers and export runner are handed.
pub fn sink(app: &AppHandle) -> Arc<dyn ProgressSink> {
    Arc::new(TauriSink(app.clone()))
}

/// Emits an event, logging rather than propagating a failure — a UI that has
/// gone away must never abort background work.
pub fn emit<T: Serialize>(app: &AppHandle, event: &str, payload: T) {
    skwad_app_core::events::emit(&TauriSink(app.clone()), event, payload);
}

pub fn notice(app: &AppHandle, level: &str, message: impl Into<String>) {
    skwad_app_core::events::notice(&TauriSink(app.clone()), level, message);
}

pub fn shoot_changed(app: &AppHandle, shoot_id: i64, reason: &str) {
    skwad_app_core::events::shoot_changed(&TauriSink(app.clone()), shoot_id, reason);
}
