//! The model registry lives in the core; this is the one piece that needs
//! Tauri — resolving where a models-bundled build put its bundle.

use std::path::Path;

use tauri::{AppHandle, Manager};

pub use skwad_app_core::models::*;

/// Copies bundled models into the library's models folder, once. See
/// [`seed_from_directory`] for the rules; this only finds the bundle.
pub fn seed_from_bundle(app: &AppHandle, destination: &Path) -> Vec<String> {
    let Ok(bundled) = app.path().resolve(BUNDLED_MODELS, tauri::path::BaseDirectory::Resource) else {
        return Vec::new();
    };
    seed_from_directory(&bundled, destination)
}
