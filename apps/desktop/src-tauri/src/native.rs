//! Commands that only mean something on the machine with the window.
//!
//! Opening a path in Explorer or Finder acts on *this* computer's desktop, so
//! it cannot be a core command a headless server would try to run. The core
//! still does the part that needs the library — resolving a catalogue media
//! reference to a safe local path — and this module does the opening.

use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::commands::{ctx_for, CommandError, Result};
use crate::state::AppState;

fn err(message: impl Into<String>) -> CommandError {
    CommandError {
        message: message.into(),
    }
}

/// Reveals a file in Explorer or Finder.
#[tauri::command]
pub fn reveal_in_folder(app: AppHandle, path: String) -> Result<()> {
    if !std::path::Path::new(&path).exists() {
        return Err(err(format!(
            "{path} is not reachable from this machine. If the library is on a server, set the shoot's share path so files can be found here."
        )));
    }
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| err(format!("could not open {path}: {e}")))
}

#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> Result<()> {
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| err(format!("could not open {path}: {e}")))
}

/// Opens one file from a loaded `.skwad` catalogue, through its approved
/// local root.
#[tauri::command]
pub async fn open_catalogue_media(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    package_id: String,
    revision_id: String,
    media_id: i64,
) -> Result<()> {
    let ctx = ctx_for(&app, state.inner());
    let target = tauri::async_runtime::spawn_blocking(move || {
        skwad_app_core::api::catalogue::resolve_catalogue_media(&ctx, package_id, revision_id, media_id)
    })
    .await
    .map_err(|e| err(format!("the command stopped unexpectedly: {e}")))?
    .map_err(CommandError::from_api)?;
    app.opener()
        .open_path(&target, None::<&str>)
        .map_err(|e| err(format!("could not open {target}: {e}")))
}

/// The network spellings of a folder on this machine, best first: a mapped
/// drive's share, a mounted volume's share, or `\\this-machine\share\…` for
/// a folder this machine shares — and how to share it when there are none.
/// A client picks folders with its own dialog, but the server is what scans
/// them, and a local path means nothing on another machine.
#[tauri::command]
pub fn network_paths(path: String, server_url: Option<String>) -> Result<crate::netpath::NetworkPaths> {
    Ok(crate::netpath::resolve(&path, server_url.as_deref()))
}
