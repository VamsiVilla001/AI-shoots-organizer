//! SKWAD Media Organiser — application wiring.

pub mod client;
pub mod commands;
pub mod db_setup;
pub mod events;
pub mod library;
pub mod models;
pub mod native;
pub mod premiere_api;
pub mod premiere_plugin;
pub mod protocol;

// The application itself lives in `skwad-app-core`, which knows nothing about
// Tauri. Re-exported under the same module names so the front-door modules
// above keep their `crate::state::AppState` paths; `events` and `models` are
// the two that needed a Tauri-flavoured shim.
pub use skwad_app_core::{export, machine, paths, pipeline, resource_monitor, settings, stages, state, worker};

use std::sync::Arc;

use skwad_database::{Database, DbError, PgConfig};
use tauri::Manager;

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
            let migration = paths::migrate_legacy_data_dir(&data_dir)?;

            // A client installation holds a server address instead of a
            // library: nothing below about databases applies to it.
            if let Some(server_url) = client::resolve_server(&data_dir) {
                return boot_client(app, data_dir, server_url);
            }

            // A team shares one library folder over the network; until an
            // administrator points this machine at one, it is this machine's
            // own application data directory.
            library::remember_app_data(&data_dir);
            let location = library::resolve(&data_dir);
            let paths = AppPaths::create_with_cache(&location.root, location.cache_root.as_deref())?;

            init_logging(&paths);
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                data = %paths.root.display(),
                source = ?location.source,
                network_share = location.network_share,
                "starting"
            );
            if migration != paths::LegacyMigration::NotNeeded {
                tracing::info!(?migration, "migrated the pre-SKWAD application library");
            }

            // `StorageMode` is gone with SQLite. It existed to downgrade the
            // journal and stretch the busy timeout when the `.db` sat on an SMB
            // share, because WAL needs shared memory an SMB client cannot
            // provide. A Postgres library is reached over TCP, so a shared
            // library is simply a server several machines connect to, and the
            // locking that mode worked around is the server's job.
            let mut db_config = PgConfig::resolve(&paths.root);
            // Fills the password from the OS credential store when nothing else
            // supplied one, which is how a machine set up through the app's own
            // settings screen gets its credential.
            db_setup::apply_saved_password(&mut db_config);
            tracing::info!(database = %db_config.describe(), "connecting to the library database");

            let db = match Database::connect(db_config.clone()) {
                Ok(db) => db,
                Err(error) => {
                    // Not a fatal error any more. Returning `Err` here made
                    // Tauri exit before a window existed, so the app appeared to
                    // hang for the connect timeout and then vanish; and even
                    // with a dialog, the only way forward was to hand-write two
                    // files. Start anyway and let the window show the setup
                    // screen — `startup_status` is what the UI checks before it
                    // calls anything needing a library.
                    let (title, detail) = describe_connection_failure(&error, &db_config, &paths.root);
                    tracing::error!(%error, "could not open the library database; starting in setup mode");
                    app.manage(db_setup::StartupStatus::NeedsDatabase {
                        settings: db_setup::DatabaseSettings::from_config(&db_config),
                        title: title.to_string(),
                        detail,
                    });
                    return Ok(());
                }
            };
            app.manage(db_setup::StartupStatus::Ready);
            // Both of these live in this machine's own app data, never the
            // (possibly shared) library folder: two machines must never claim
            // jobs under one id, and a laptop must never inherit the
            // server's accelerator choice.
            let machine_settings_file = settings::machine_settings_path(&data_dir);
            let settings = AppSettings::load(&db, &machine_settings_file)
                .unwrap_or_default()
                .sanitised();
            let machine_id = machine::load_or_create(&data_dir);
            let state = Arc::new(AppState::new(
                db,
                paths,
                settings,
                protocol::url_base(),
                machine_id,
                machine_settings_file,
            ));
            app.manage(Arc::clone(&state));

            // The roster lives in the library folder, so it is prepared once the
            // library is open and before anyone reaches the sign-in screen.
            skwad_app_core::api::catalogue::ensure_local_auth(&state);

            // Lets an external process (the Premiere Pro panel) read Collections
            // over loopback HTTP — see premiere_api.rs for why that's necessary.
            premiere_api::start(Arc::clone(&state));

            // Puts the Premiere panel on this machine without anyone having to
            // install a plugin by hand — see premiere_plugin.rs. Runs on its own
            // thread and cannot fail startup.
            premiere_plugin::ensure_installed(app.handle().clone());

            // A models-bundled build installs them on first launch. On its own
            // thread because that is ~191 MB to copy and blocking here would
            // hold the window back on exactly the launch where someone is
            // deciding whether the app works. Nothing needs them this early:
            // a fresh library has no shoots, so no analysis job can be waiting
            // on a model, and by the time anyone imports one this is long done.
            let seed_handle = app.handle().clone();
            let models_dir = state.paths.models.clone();
            std::thread::Builder::new()
                .name("skwad-models-seed".into())
                .spawn(move || {
                    let installed = models::seed_from_bundle(&seed_handle, &models_dir);
                    if !installed.is_empty() {
                        tracing::info!(models = ?installed, "installed bundled models");
                    }
                })
                .ok();

            // Workers start immediately so an import interrupted by a previous
            // quit resumes without the user having to ask (§18).
            let pool = worker::WorkerPool::start(events::sink(app.handle()), Arc::clone(&state));
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
                if let Some(mode) = window.app_handle().try_state::<Arc<client::ClientMode>>() {
                    mode.stop_worker();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            // application
            db_setup::startup_status,
            db_setup::database_settings,
            db_setup::test_database_connection,
            db_setup::save_database_connection,
            db_setup::restart_for_database_change,
            client::client_status,
            client::set_server_url,
            client::restart_for_client_change,
            client::store_machine_enrolment,
            client::forget_machine_enrolment,
            client::set_worker_enabled,
            client::update_worker_settings,
            commands::list_machines,
            commands::enrol_machine,
            commands::revoke_machine,
            commands::app_info,
            commands::get_settings,
            commands::update_settings,
            commands::model_status,
            commands::embedding_cohorts,
            commands::reembed_stale_faces,
            commands::list_projects,
            commands::save_project,
            commands::delete_project,
            commands::replace_project_members,
            commands::catalogue_session_status,
            commands::sign_in_skwad,
            commands::change_initial_password,
            commands::list_local_users,
            commands::create_local_user,
            commands::update_local_user,
            commands::reset_local_user_password,
            commands::delete_local_user,
            library::get_library_location,
            library::set_library_location,
            library::restart_for_library_change,
            commands::preview_roster_file,
            commands::import_roster,
            commands::roster_summary,
            commands::list_roster,
            commands::search_roster,
            commands::resolve_roster_name,
            commands::clear_roster,
            commands::sign_out_skwad,
            commands::clear_authenticated_session,
            commands::get_user_profile,
            commands::update_user_profile,
            commands::publish_skwad,
            commands::load_skwad,
            commands::approve_catalogue_library,
            commands::list_loaded_catalogues,
            commands::list_catalogue_groups,
            commands::list_catalogue_media,
            native::open_catalogue_media,
            // shoots
            commands::list_shoots,
            commands::get_shoot,
            commands::create_shoot,
            commands::rename_shoot,
            commands::delete_shoot_index,
            commands::clear_selected_scanned_data,
            commands::clear_scanned_data,
            commands::resume_processing,
            commands::pause_processing,
            commands::cancel_processing,
            commands::reanalyse_shoot,
            commands::get_progress,
            commands::get_shoot_telemetry,
            commands::get_shoot_storage,
            commands::list_failed_jobs,
            // media
            commands::list_media,
            commands::get_media,
            commands::media_faces,
            commands::set_media_editorial,
            native::reveal_in_folder,
            native::open_path,
            native::network_path,
            // players
            commands::list_people,
            commands::list_enrolled_people,
            commands::reference_library_shoot_id,
            commands::create_person,
            commands::enroll_person,
            commands::enroll_people_from_directory,
            commands::find_person_media,
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
            commands::add_manual_face,
            commands::name_face,
            // video
            commands::video_timelines,
            commands::video_sample_frames,
            // export
            commands::preview_export,
            commands::start_export,
            commands::cancel_export,
            commands::list_exports,
            // premiere
            commands::send_media_to_premiere,
            commands::send_collection_to_premiere,
            premiere_plugin::premiere_panel_status,
            premiere_plugin::install_premiere_panel,
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

/// Starts the app as a client of `server_url`: no database, no library folder,
/// no local job queue. The webview talks to the server over HTTP; the only
/// thing this process may run is the worker, when the machine is enrolled
/// and the person has switched it on.
fn boot_client(app: &mut tauri::App, data_dir: std::path::PathBuf, server_url: String) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::create(&data_dir)?;
    init_logging(&paths);
    let config = client::load(&data_dir);
    let machine_id = machine::load_or_create(&data_dir);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        server = %server_url,
        machine = %machine_id,
        enrolled = config.machine_token.is_some(),
        worker = config.worker_enabled,
        "starting as a client"
    );
    app.manage(db_setup::StartupStatus::Client {
        server_url: server_url.clone(),
        machine_id: machine_id.clone(),
        machine_name: config.machine_name.clone(),
        worker_enabled: config.worker_enabled,
    });
    let mode = Arc::new(client::ClientMode::new(data_dir, paths, machine_id, config.clone()));
    app.manage(Arc::clone(&mode));

    // Bundled models go into this machine's own folder so the worker has
    // them without a download when they match the server's pair.
    let seed_handle = app.handle().clone();
    let models_dir = mode.paths.models.clone();
    std::thread::Builder::new()
        .name("skwad-models-seed".into())
        .spawn(move || {
            let installed = models::seed_from_bundle(&seed_handle, &models_dir);
            if !installed.is_empty() {
                tracing::info!(models = ?installed, "installed bundled models");
            }
        })
        .ok();

    if config.worker_enabled && config.machine_token.is_some() {
        if let Err(error) = mode.start_worker(events::sink(app.handle())) {
            tracing::warn!(error = %error.message, "worker mode did not start");
        }
    }
    Ok(())
}

/// Logs to a rolling file in the app data directory, and to the console during
/// development. Kept lightweight, as §25 asks.
/// Where libpq — and so `PgConfig` — looks for the password.
///
/// Named rather than hard-coded into the messages because it differs by
/// platform, and a message that points at the wrong file is worse than one that
/// points at none.
/// Turns a connection failure into a heading and a short explanation for the
/// setup screen.
///
/// Deliberately short now. This text sits directly above a form with the very
/// fields it is about, so it only has to say which of them is wrong — earlier
/// versions told people which files to hand-edit, which is the problem the
/// setup screen exists to remove.
fn describe_connection_failure(
    error: &DbError,
    config: &PgConfig,
    _library_root: &std::path::Path,
) -> (&'static str, String) {
    let where_it_looked = config.describe();

    if error.is_missing_credential() {
        return (
            "SKWAD needs a database password",
            format!("A database server answered at {where_it_looked}, but SKWAD has no password for it."),
        );
    }

    if error.is_rejected() {
        return (
            "SKWAD was refused by its library",
            format!(
                "The server at {where_it_looked} answered and refused the connection.\n\
                 Usually the password is wrong, the user does not exist, or the database does \
                 not exist on that server."
            ),
        );
    }

    if config.is_local() {
        (
            "SKWAD cannot find its library",
            format!(
                "Nothing answered at {where_it_looked}.\n\
                 If the library is on another machine, enter its address below. If it should be \
                 on this one, PostgreSQL needs to be installed and running here."
            ),
        )
    } else {
        (
            "SKWAD cannot reach its library",
            format!(
                "Nothing answered at {where_it_looked}.\n\
                 Check that machine is on, that its PostgreSQL service is running, and that its \
                 firewall allows this one to reach port {}.",
                config.port
            ),
        )
    }
}
fn init_logging(paths: &AppPaths) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_env("SKWAD_LOG").unwrap_or_else(|_| EnvFilter::new("info,teo=debug"));

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
