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

/// The network spelling of a path on a mapped drive — `Z:\shoots\day1`
/// becomes `\\nas\media\shoots\day1` — or `None` when the path is not on a
/// mapped drive. A client picks folders with its own dialog, but the server
/// is what scans them, and a drive letter means nothing on another machine.
#[tauri::command]
pub fn network_path(path: String) -> Result<Option<String>> {
    Ok(to_network_path(&path))
}

#[cfg(windows)]
fn to_network_path(path: &str) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;

    let bytes = path.as_bytes();
    let is_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if !is_drive || path.starts_with("\\\\") {
        return None;
    }
    let drive: Vec<u16> = path[..2].encode_utf16().chain(std::iter::once(0)).collect();
    let mut remote = vec![0u16; 1024];
    let mut length = remote.len() as u32;
    // SAFETY: both buffers outlive the call and `length` is their capacity.
    let status = unsafe { WNetGetConnectionW(drive.as_ptr(), remote.as_mut_ptr(), &mut length) };
    if status != 0 {
        return None;
    }
    let end = remote.iter().position(|&c| c == 0).unwrap_or(remote.len());
    let share = String::from_utf16_lossy(&remote[..end]);
    let rest = path[2..].trim_start_matches(['\\', '/']);
    Some(if rest.is_empty() {
        share
    } else {
        format!("{}\\{}", share.trim_end_matches('\\'), rest)
    })
}

#[cfg(not(windows))]
fn to_network_path(_path: &str) -> Option<String> {
    None
}
