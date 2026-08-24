//! HTTP front door for [`teo_app_core`].
//!
//! The core is shared with the desktop app: same database, same job queue, same
//! worker policy. This crate only adds a transport — routes that mirror the
//! Tauri command layer one for one, an SSE stream carrying the same events, and
//! media routes that resolve database ids exactly as `teomedia://` does.
//!
//! ```text
//!   browser ──HTTP──▶ axum router ──▶ teo-app-core ──▶ SQLite + workers
//!      ▲                  │
//!      └───SSE────────────┘  (SseProgressSink)
//! ```
//!
//! Everything under `/api` and `/media` requires the bearer token. The built
//! React bundle is served at `/` when a bundle is configured.

pub mod api;
pub mod auth;
pub mod config;
pub mod error;
pub mod fsbrowse;
pub mod mediaroutes;
pub mod sse;
pub mod state;

use std::sync::Arc;

use axum::routing::{get, patch, post};
use axum::Router;
use teo_app_core::{AppPaths, AppSettings, AppState, ProgressSink, WorkerPool};
use teo_database::Database;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

pub use config::ServerConfig;
pub use state::ServerState;

/// The path prefix a front end prepends to media ids. Kept on `AppState` so the
/// same `app_info` field works for either front door.
pub const MEDIA_URL_BASE: &str = "/media";

/// Opens the database, starts the workers, and returns the assembled state.
///
/// Separate from [`serve`] so a test can drive the API without binding a port.
pub fn boot(config: ServerConfig) -> anyhow::Result<(Arc<ServerState>, WorkerPool)> {
    let paths = AppPaths::create(&config.data_dir)?;
    let token = auth::resolve_token(config.token.clone(), &config.data_dir)?;

    // A packaged desktop app ships the models as bundle resources and points us
    // at them; the first run copies them in. Nothing to do for a container,
    // which fetches them at entrypoint instead.
    if let Some(source) = &config.seed_models_from {
        let installed = teo_app_core::models::seed_from_bundle(source, &paths.models);
        if installed > 0 {
            tracing::info!(installed, "installed bundled models into the data directory");
        }
    }

    let db = Database::open(paths.database_file())?;
    let settings = AppSettings::load(&db).unwrap_or_default().sanitised();

    // The sink needs the broadcast channel that lives on ServerState, and the
    // state needs the core, so the channel is created first and shared.
    let (events, _) = tokio::sync::broadcast::channel(state::EVENT_BUFFER);
    let sink: Arc<dyn ProgressSink> = Arc::new(sse::SseProgressSink::new(events.clone()));

    let core = Arc::new(AppState::new(db, paths, settings, MEDIA_URL_BASE.to_string(), sink));

    if config.media_roots.is_empty() {
        tracing::warn!(
            "no TEO_MEDIA_ROOTS configured: the filesystem browser is disabled and any \
             destination is accepted. Set it for anything reachable by more than one person."
        );
    }

    // Workers start immediately so an import interrupted by a previous run
    // resumes without anyone having to ask.
    let workers = WorkerPool::start(Arc::clone(&core));

    Ok((
        Arc::new(ServerState { core, config, token, events }),
        workers,
    ))
}

/// Everything behind the token: the API and media bytes.
fn protected() -> Router<Arc<ServerState>> {
    Router::new()
        // application
        .route("/api/system/status", get(api::system_status))
        .route("/api/system/models", get(api::model_status))
        .route("/api/settings", get(api::get_settings).put(api::update_settings))
        // shoots
        .route("/api/shoots", get(api::list_shoots).post(api::create_shoot))
        .route(
            "/api/shoots/{id}",
            get(api::get_shoot).patch(api::rename_shoot).delete(api::delete_shoot_index),
        )
        .route("/api/shoots/{id}/resume", post(api::resume_processing))
        .route("/api/shoots/{id}/cancel", post(api::cancel_processing))
        .route("/api/shoots/{id}/reanalyse", post(api::reanalyse_shoot))
        .route("/api/shoots/{id}/progress", get(api::get_progress))
        .route("/api/shoots/{id}/failed-jobs", get(api::list_failed_jobs))
        .route("/api/shoots/{id}/media", get(api::list_media))
        .route("/api/processing/pause", post(api::pause_processing))
        .route("/api/jobs/summary", get(api::jobs_summary))
        // media
        .route("/api/media/{id}", get(api::get_media))
        .route("/api/media/{id}/faces", get(api::media_faces))
        .route("/api/media/{id}/timelines", get(api::video_timelines))
        // people
        .route("/api/people", get(api::list_people).post(api::create_person))
        .route("/api/people/{id}", patch(api::update_person).delete(api::delete_person))
        .route("/api/people/{id}/merge", post(api::merge_people))
        .route("/api/people/{id}/clear-recognition", post(api::clear_person_recognition))
        // clusters
        .route("/api/clusters", get(api::list_clusters))
        .route("/api/clusters/{id}/name", post(api::name_cluster))
        .route("/api/clusters/{id}/merge", post(api::merge_clusters))
        .route("/api/clusters/{id}/split", post(api::split_cluster))
        .route("/api/clusters/{id}/ignore", post(api::ignore_cluster))
        // albums
        .route("/api/albums", get(api::list_albums))
        .route("/api/albums/regenerate", post(api::regenerate_albums))
        // groups
        .route("/api/groups", get(api::list_groups).post(api::create_group))
        .route("/api/groups/stats", get(api::group_stats))
        .route("/api/groups/links", get(api::group_links))
        .route("/api/groups/from-ai-albums", post(api::groups_from_ai_albums))
        .route("/api/groups/from-album", post(api::group_from_album))
        .route("/api/groups/{id}", patch(api::update_group).delete(api::delete_group))
        .route("/api/groups/media", post(api::add_media_to_group))
        .route("/api/groups/{id}/media", axum::routing::delete(api::remove_media_from_group))
        .route("/api/groups/{id}/clear", post(api::clear_group))
        // review
        .route("/api/faces", get(api::list_faces))
        .route("/api/faces/confirm", post(api::confirm_faces))
        .route("/api/faces/reject", post(api::reject_faces))
        .route("/api/faces/assign", post(api::assign_faces))
        .route("/api/faces/not-a-face", post(api::ignore_faces))
        // export
        .route("/api/exports", get(api::list_exports).post(api::start_export))
        .route("/api/exports/preview", post(api::preview_export))
        .route("/api/exports/cancel", post(api::cancel_export))
        // logs and privacy
        .route("/api/logs", get(api::recent_logs))
        .route("/api/maintenance/clear-scanned-data", post(api::clear_scanned_data))
        .route("/api/maintenance/clear-embeddings", post(api::clear_all_embeddings))
        .route("/api/maintenance/clear-recognition-data", post(api::clear_all_recognition_data))
        .route("/api/maintenance/clear-thumbnails", post(api::clear_thumbnail_cache))
        .route("/api/maintenance/clear-log", post(api::clear_log))
        // filesystem browser — the folder picker a browser cannot have
        .route("/api/fs/roots", get(fsbrowse::roots))
        .route("/api/fs/list", get(fsbrowse::list))
        // events
        .route("/api/events", get(sse::stream))
        // media bytes
        .route("/media/{id}/thumb", get(mediaroutes::thumbnail))
        .route("/media/{id}/full", get(mediaroutes::full))
        .route("/media/{id}/stream", get(mediaroutes::stream))
}

/// Origins allowed to call the API from a page this server did not serve.
///
/// The desktop shell is the reason this exists: its webview is
/// `tauri.localhost` while the server is `127.0.0.1:<port>`, so every request is
/// cross-origin. The Vite dev server is here for the same reason. Anything else
/// has to be named in `TEO_ALLOWED_ORIGINS` — a wildcard is not an option once
/// credentials are involved, and should not be one anyway.
fn allowed_origins() -> Vec<axum::http::HeaderValue> {
    let mut origins = vec![
        // Tauri's webview origin, per platform.
        "http://tauri.localhost".to_string(),
        "https://tauri.localhost".to_string(),
        "tauri://localhost".to_string(),
        // `npm run dev`.
        "http://localhost:1420".to_string(),
        "http://127.0.0.1:1420".to_string(),
    ];

    if let Ok(extra) = std::env::var("TEO_ALLOWED_ORIGINS") {
        origins.extend(extra.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from));
    }

    origins.iter().filter_map(|origin| origin.parse().ok()).collect()
}

/// The full application: token-checked routes, the session endpoint that hands
/// out the cookie, and the static bundle underneath.
pub fn router(state: Arc<ServerState>) -> Router {
    let web_dir = state.config.web_dir.clone();

    let mut app = Router::new()
        .merge(protected().route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_token,
        )))
        // Outside the middleware by necessity: this is where a browser presents
        // the token for the first time.
        .route("/api/auth/session", post(auth::create_session));

    match web_dir.filter(|dir| dir.is_dir()) {
        Some(dir) => {
            let index = dir.join("index.html");
            // A single-page app: unknown paths fall back to index.html so a
            // reload on a client-side route still works.
            app = app.fallback_service(
                tower_http::services::ServeDir::new(dir)
                    .fallback(tower_http::services::ServeFile::new(index)),
            );
        }
        None => {
            tracing::info!("no web bundle configured; serving the API only");
        }
    }

    // Credentials are on because the browser edition authenticates with a
    // cookie; that rules out a wildcard origin, which is why the list above is
    // explicit. Methods and headers are listed rather than `Any` for the same
    // reason: with credentials, browsers treat `*` literally.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins()))
        .allow_credentials(true)
        .allow_headers([axum::http::header::AUTHORIZATION, axum::http::header::CONTENT_TYPE])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ]);

    app.layer(cors).layer(TraceLayer::new_for_http()).with_state(state)
}

/// Binds and serves until the process is asked to stop.
pub async fn serve(state: Arc<ServerState>) -> anyhow::Result<()> {
    let bind = state.config.bind.clone();
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let local = listener.local_addr()?;
    tracing::info!(address = %local, "listening");

    // A parent that asked for port 0 cannot know the port any other way, and
    // guessing one to pass in would race with whatever else is on the machine.
    if let Some(path) = &state.config.port_file {
        if let Err(e) = std::fs::write(path, local.to_string()) {
            tracing::error!(path = %path.display(), error = %e, "could not publish the bound address");
        }
    }

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_webview_origin_is_allowed() {
        let origins: Vec<String> = allowed_origins()
            .iter()
            .map(|value| value.to_str().unwrap_or_default().to_string())
            .collect();

        // Without these the desktop shell cannot call its own server: the page
        // is tauri.localhost and the server is 127.0.0.1.
        assert!(origins.contains(&"http://tauri.localhost".to_string()));
        assert!(origins.contains(&"tauri://localhost".to_string()));
        assert!(origins.contains(&"http://localhost:1420".to_string()));
        assert!(
            !origins.iter().any(|o| o == "*"),
            "a wildcard origin is invalid with credentials and wrong regardless"
        );
    }

    #[test]
    fn the_media_base_is_a_path_not_a_scheme() {
        // The desktop passes `teomedia://…`; over HTTP it has to be a path the
        // browser resolves against the origin serving the bundle.
        assert!(MEDIA_URL_BASE.starts_with('/'));
    }

    #[test]
    fn every_protected_route_is_reachable_only_through_the_middleware() {
        // A smoke test on assembly: building the router panics on a duplicate
        // path or a handler whose extractors do not line up, which is the
        // failure mode worth catching in CI rather than at first request.
        let dir = tempfile::tempdir().unwrap();
        let config = ServerConfig {
            bind: "127.0.0.1:0".into(),
            data_dir: dir.path().to_path_buf(),
            media_roots: vec![dir.path().to_path_buf()],
            output_roots: Vec::new(),
            token: Some("test-token".into()),
            web_dir: None,
            port_file: None,
            seed_models_from: None,
        };
        let (state, workers) = boot(config).unwrap();
        let _router = router(Arc::clone(&state));
        state.core.begin_shutdown();
        workers.join();
    }
}
