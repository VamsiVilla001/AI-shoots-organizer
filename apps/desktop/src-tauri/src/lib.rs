//! Esports AI Media Organiser — application wiring.

pub mod commands;
pub mod events;
pub mod export;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod protocol;
pub mod settings;
pub mod stages;
pub mod state;
pub mod worker;

use std::sync::Arc;

use tauri::Manager;
use teo_database::Database;

use crate::paths::AppPaths;
use crate::settings::AppSettings;
use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol(protocol::SCHEME, |ctx, request, responder| {
            protocol::handle(ctx.app_handle(), request, responder);
        })
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("could not resolve the application data directory: {e}"))?;
            let paths = AppPaths::create(&data_dir)?;

            init_logging(&paths);
            tracing::info!(version = env!("CARGO_PKG_VERSION"), data = %paths.root.display(), "starting");

            // A packaged build ships the ONNX models as bundle resources, so an
            // installed app is usable without whoever installed it running a
            // fetch script. Development builds have no such resources and skip
            // this silently.
            if let Ok(resources) = app.path().resource_dir() {
                let installed = models::seed_from_bundle(&resources.join("models"), &paths.models);
                if installed > 0 {
                    tracing::info!(installed, "installed bundled models into the app data folder");
                }
            }

            let db = Database::open(paths.database_file())
                .map_err(|e| format!("could not open the database: {e}"))?;
            let settings = AppSettings::load(&db)
                .unwrap_or_default()
                .sanitised();

            let state = Arc::new(AppState::new(db, paths, settings, protocol::url_base()));
            app.manage(Arc::clone(&state));

            // Workers start immediately so an import interrupted by a previous
            // quit resumes without the user having to ask (§18).
            let pool = worker::WorkerPool::start(app.handle().clone(), Arc::clone(&state));
            app.manage(Mutex::new(Some(pool)));

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Tell the workers to stop, then let them finish the job they
                // are on. Anything still queued is picked up next launch.
                if let Some(state) = window.app_handle().try_state::<Arc<AppState>>() {
                    state.begin_shutdown();
                }
                if let Some(pool) = window.app_handle().try_state::<Mutex<Option<worker::WorkerPool>>>() {
                    if let Some(pool) = pool.lock().take() {
                        pool.join();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            // application
            commands::app_info,
            commands::get_settings,
            commands::update_settings,
            commands::model_status,
            // shoots
            commands::list_shoots,
            commands::get_shoot,
            commands::create_shoot,
            commands::rename_shoot,
            commands::delete_shoot_index,
            commands::clear_scanned_data,
            commands::resume_processing,
            commands::pause_processing,
            commands::cancel_processing,
            commands::reanalyse_shoot,
            commands::get_progress,
            commands::list_failed_jobs,
            // media
            commands::list_media,
            commands::get_media,
            commands::media_faces,
            commands::reveal_in_folder,
            commands::open_path,
            // players
            commands::list_people,
            commands::create_person,
            commands::rename_person,
            commands::update_person,
            commands::merge_people,
            commands::delete_person,
            commands::clear_person_recognition,
            // clusters
            commands::list_clusters,
            commands::name_cluster,
            commands::merge_clusters,
            commands::split_cluster,
            commands::ignore_cluster,
            // albums
            commands::list_albums,
            commands::regenerate_albums,
            // groups (the editor's own sorting)
            commands::list_groups,
            commands::group_stats,
            commands::group_links,
            commands::create_group,
            commands::rename_group,
            commands::update_group,
            commands::delete_group,
            commands::add_media_to_group,
            commands::remove_media_from_group,
            commands::clear_group,
            commands::groups_from_ai_albums,
            commands::group_from_album,
            // review
            commands::list_faces,
            commands::confirm_faces,
            commands::reject_faces,
            commands::assign_faces,
            commands::ignore_faces,
            // video
            commands::video_timelines,
            // export
            commands::preview_export,
            commands::start_export,
            commands::cancel_export,
            commands::list_exports,
            // logs and privacy
            commands::recent_logs,
            commands::clear_all_embeddings,
            commands::clear_all_recognition_data,
            commands::clear_thumbnail_cache,
            commands::clear_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}

use parking_lot::Mutex;

/// Logs to a rolling file in the app data directory, and to the console during
/// development. Kept lightweight, as §25 asks.
fn init_logging(paths: &AppPaths) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_env("TEO_LOG").unwrap_or_else(|_| EnvFilter::new("info,teo=debug"));

    let file_layer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())
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
