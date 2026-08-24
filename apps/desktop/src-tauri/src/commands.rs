//! What only the shell can do.
//!
//! Every command that reads or writes the library now lives in `teo-server` and
//! is reached over HTTP, so the desktop and the NAS edition run one
//! implementation rather than two. What is left is the handful of things a
//! server on someone else's machine could never do: tell the front end where
//! its private server is, and open the operating system's own file manager.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::supervisor::{ServerStatus, Supervisor};

/// Errors cross the IPC boundary as a plain message; the UI shows it verbatim,
/// so the text has to be something a person can act on.
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
}

impl<E: std::fmt::Display> From<E> for CommandError {
    fn from(e: E) -> Self {
        CommandError { message: e.to_string() }
    }
}

pub type Result<T> = std::result::Result<T, CommandError>;

/// Where the private server is listening, and the token for it.
///
/// The front end calls this at boot and again if a request fails, because a
/// restarted server comes back on a different port.
#[tauri::command]
pub fn server_status(supervisor: State<'_, Arc<Supervisor>>) -> Result<ServerStatus> {
    Ok(supervisor.status())
}

/// Reveals a file in Explorer or Finder.
#[tauri::command]
pub fn reveal_in_folder(app: AppHandle, path: String) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| CommandError { message: format!("could not open {path}: {e}") })
}

#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| CommandError { message: format!("could not open {path}: {e}") })
}
