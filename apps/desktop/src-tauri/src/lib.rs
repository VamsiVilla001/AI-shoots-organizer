//! Esports AI Media Organiser — the desktop shell.
//!
//! The shell is a window, a menu, a lifecycle and a supervisor. Everything the
//! application actually does lives in `teo-app-core` and is reached through
//! `teo-server`, which this process starts privately on loopback. One
//! implementation of every command serves both editions.
//!
//! The app data layout is unchanged from 0.1.0 — the server is pointed at the
//! same `com.teorganiser.desktop` directory — so an upgrade opens the existing
//! `media.db` and its migrations continue from where they were.

pub mod commands;
pub mod supervisor;

use std::sync::Arc;

use tauri::Manager;

use crate::supervisor::Supervisor;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("could not resolve the application data directory: {e}"))?;
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;

            init_logging(&data_dir);
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                data = %data_dir.display(),
                "starting"
            );

            // A packaged build keeps the ONNX models in its resources; the
            // server installs them into the data directory on first run.
            let resources = app.path().resource_dir().ok();

            // Blocking here is deliberate: the window has nothing useful to show
            // until the server is up, and a failure has to reach the UI as a
            // status rather than as an empty screen.
            let supervisor = Arc::new(Supervisor::start(data_dir, resources));
            supervisor.supervise();
            app.manage(Arc::clone(&supervisor));

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // The child holds the database; leaving it running would keep a
                // lock on it and a port open after the window is gone.
                if let Some(supervisor) = window.app_handle().try_state::<Arc<Supervisor>>() {
                    supervisor.stop();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::server_status,
            commands::reveal_in_folder,
            commands::open_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}

/// Logs to a rolling file in the app data directory, and to the console during
/// development. The server's own output is forwarded into the same file, so one
/// log tells the whole story.
fn init_logging(data_dir: &std::path::Path) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let logs = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&logs);

    let filter = EnvFilter::try_from_env("TEO_LOG").unwrap_or_else(|_| EnvFilter::new("info,teo=debug"));

    let file_layer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("teo.log"))
        .ok()
        // `MakeWriter` is implemented for the standard-library mutex, not
        // parking_lot's, so this one deliberately differs from the rest of the
        // crate.
        .map(|file| fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(file)));

    let registry = tracing_subscriber::registry().with(filter).with(fmt::layer());

    let result = match file_layer {
        Some(layer) => registry.with(layer).try_init(),
        None => registry.try_init(),
    };

    if let Err(e) = result {
        eprintln!("logging is already initialised: {e}");
    }
}
