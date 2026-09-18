//! SKWAD Media Organiser — application wiring.

pub mod catalogue;
pub mod commands;
pub mod events;
pub mod export;
pub mod library;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod premiere_api;
pub mod premiere_plugin;
pub mod protocol;
pub mod resource_monitor;
pub mod roster;
pub mod settings;
pub mod stages;
pub mod state;
pub mod storage;
pub mod worker;

use std::sync::Arc;

use skwad_database::{Database, DbError, PgConfig};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

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
            let db_config = PgConfig::resolve(&paths.root);
            tracing::info!(database = %db_config.describe(), "connecting to the library database");
            let db = match Database::connect(db_config.clone()) {
                Ok(db) => db,
                Err(error) => {
                    // Returning `Err` from `setup` makes Tauri exit before any
                    // window exists, so the message goes only to the log and
                    // the app looks like it hung for the connect timeout and
                    // then vanished. Say it to the person's face instead —
                    // this is the most likely first-run failure now that the
                    // index lives on a server.
                    let (title, detail) = describe_connection_failure(&error, &db_config, &paths.root);
                    tracing::error!(%error, "could not open the library database");
                    app.dialog()
                        .message(&detail)
                        .kind(MessageDialogKind::Error)
                        .title(title)
                        .blocking_show();
                    return Err(detail.into());
                }
            };
            let settings = AppSettings::load(&db).unwrap_or_default().sanitised();

            let state = Arc::new(AppState::new(db, paths, settings, protocol::url_base()));
            app.manage(Arc::clone(&state));

            // The roster lives in the library folder, so it is prepared once the
            // library is open and before anyone reaches the sign-in screen.
            catalogue::ensure_local_auth(&state);

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
            commands::list_projects,
            commands::save_project,
            commands::delete_project,
            commands::replace_project_members,
            catalogue::catalogue_session_status,
            catalogue::sign_in_skwad,
            catalogue::change_initial_password,
            catalogue::list_local_users,
            catalogue::create_local_user,
            catalogue::update_local_user,
            catalogue::reset_local_user_password,
            catalogue::delete_local_user,
            library::get_library_location,
            library::set_library_location,
            library::restart_for_library_change,
            roster::preview_roster_file,
            roster::import_roster,
            roster::roster_summary,
            roster::list_roster,
            roster::search_roster,
            roster::resolve_roster_name,
            roster::clear_roster,
            catalogue::sign_out_skwad,
            catalogue::clear_authenticated_session,
            catalogue::get_user_profile,
            catalogue::update_user_profile,
            catalogue::publish_skwad,
            catalogue::load_skwad,
            catalogue::approve_catalogue_library,
            catalogue::list_loaded_catalogues,
            catalogue::list_catalogue_groups,
            catalogue::list_catalogue_media,
            catalogue::open_catalogue_media,
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
            storage::get_shoot_storage,
            commands::list_failed_jobs,
            // media
            commands::list_media,
            commands::get_media,
            commands::media_faces,
            commands::set_media_editorial,
            commands::reveal_in_folder,
            commands::open_path,
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

/// Logs to a rolling file in the app data directory, and to the console during
/// development. Kept lightweight, as §25 asks.
/// Where libpq — and so `PgConfig` — looks for the password.
///
/// Named rather than hard-coded into the messages because it differs by
/// platform, and a message that points at the wrong file is worse than one that
/// points at none.
fn pgpass_path() -> std::path::PathBuf {
    if let Some(explicit) = std::env::var_os("PGPASSFILE") {
        return std::path::PathBuf::from(explicit);
    }
    #[cfg(windows)]
    {
        let root = std::env::var_os("APPDATA").unwrap_or_default();
        std::path::PathBuf::from(root).join("postgresql").join("pgpass.conf")
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join(".pgpass")
    }
}

/// Turns a connection failure into something the person in front of the machine
/// can act on.
///
/// The three cases are genuinely different problems with different fixes, and
/// the raw driver error distinguishes none of them: "connection refused" reads
/// the same whether the server is off, the firewall is shut, or this machine
/// was never told where to look.
fn describe_connection_failure(
    error: &DbError,
    config: &PgConfig,
    library_root: &std::path::Path,
) -> (&'static str, String) {
    let where_it_looked = config.describe();
    let config_file = library_root.join("database.json");

    // Checked before unavailability: a server that answered and then refused
    // for want of a password looks identical to an unreachable one in the
    // driver's error, and sending someone to check a firewall that is fine is
    // worse than saying nothing.
    if error.is_missing_credential() {
        return (
            "SKWAD needs a database password",
            format!(
                "A database server answered at {where_it_looked}, but SKWAD has no password for it.\n\n\
                 The password is read from:\n  {}\n\n\
                 Add a line for this server — host:port:database:user:password — for example:\n  \
                   {}:{}:{}:{}:<the password>\n\n\
                 Details: {error}",
                pgpass_path().display(),
                config.host,
                config.port,
                config.database,
                config.user
            ),
        );
    }

    if error.is_rejected() {
        return (
            "SKWAD was refused by its library",
            format!(
                "The database server at {where_it_looked} answered, and refused the connection.\n\n\
                 Usually one of: the password is wrong, the user does not exist, or the\n\
                 database does not exist on that server.\n\n\
                 The password is read from {}.\n\n\
                 Details: {error}",
                pgpass_path().display()
            ),
        );
    }

    // Nothing answered. Which advice is useful depends entirely on whether this
    // machine is meant to host the library or reach one elsewhere.
    //
    // `SKWAD_DATABASE_URL` counts as having been told: it overrides everything,
    // so pointing at database.json when that is what set the address would send
    // someone to edit a file that is not being read.
    let told_by_env = std::env::var_os("SKWAD_DATABASE_URL").is_some();
    if config.is_local() && !config_file.exists() && !told_by_env {
        (
            "SKWAD has not been told where its library is",
            format!(
                "SKWAD could not reach a database, and this machine has not been told where to find one.\n\n\
                 It looked for {where_it_looked}, which is the default.\n\n\
                 If the library lives on ANOTHER machine, create:\n  \
                   {}\n  \
                 containing that machine's address — see docs/deployment.md.\n\n\
                 If the library should live on THIS machine, install PostgreSQL 15+ and run\n  \
                   npm run db:setup\n\n\
                 Details: {error}",
                config_file.display()
            ),
        )
    } else if config.is_local() {
        (
            "SKWAD cannot reach its library",
            format!(
                "SKWAD could not reach its library database at {where_it_looked}.\n\n\
                 The database server on this machine does not appear to be running.\n\
                 Start the \"postgresql\" service, then open SKWAD again.\n\n\
                 Details: {error}"
            ),
        )
    } else {
        (
            "SKWAD cannot reach its library",
            format!(
                "SKWAD could not reach its library database at {where_it_looked}.\n\n\
                 That machine is configured in:\n  {}\n\n\
                 Check, in this order:\n  \
                   1. that machine is on and its PostgreSQL service is running\n  \
                   2. its firewall allows TCP {} from this machine\n  \
                   3. its pg_hba.conf permits this machine's address\n\n\
                 From this machine, `Test-NetConnection {} -Port {}` should succeed.\n\n\
                 Details: {error}",
                config_file.display(),
                config.port,
                config.host,
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
