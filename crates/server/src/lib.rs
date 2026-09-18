//! The headless front door for [`skwad_app_core`].
//!
//! One machine owns the library, the database and the finishing stages; every
//! other machine is a client that talks to this. The core is shared with the
//! desktop app — same database, same job queue, same worker policy — and this
//! crate only adds a transport:
//!
//! ```text
//!   client ──HTTPS──▶ axum router ──▶ skwad-app-core ──▶ PostgreSQL + workers
//!      ▲                  │
//!      └───SSE────────────┘  (SseProgressSink, coalesced)
//! ```
//!
//! * `POST /api/invoke/{command}` runs a command from the core's registry by
//!   name with the same JSON arguments the Tauri bridge sends. There is no
//!   second route table to keep in step.
//! * `GET /api/events` streams the same events the desktop listens for.
//! * `GET /media/{kind}/{id}` serves the same bytes `skwadmedia://` does.
//! * `GET /health` says what is wrong with the box.
//!
//! Everything under `/api` (except the health report) and `/media` needs a
//! signed-in session; every `/api` request must also say which API version
//! it speaks.

pub mod auth;
pub mod config;
pub mod error;
pub mod fsbrowse;
pub mod health;
pub mod mediaroutes;
pub mod sse;
pub mod state;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use skwad_app_core::api::{self, Ctx, Session};
use skwad_app_core::{AppPaths, AppSettings, AppState, ProgressSink, WorkerPool};
use skwad_database::{Database, PgConfig};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::auth::{SessionRecord, SessionStore};
pub use crate::config::ServerConfig;
use crate::error::{blocking, ApiError, ApiResult};
pub use crate::state::ServerState;

/// The media base a client is told about. Same route shape as the desktop's
/// scheme (`/thumb/{id}` and so on), rooted at `/media`.
pub const MEDIA_URL_BASE: &str = "/media";

/// Commands a caller may run before signing in.
const PUBLIC_COMMANDS: &[&str] = &["catalogue_session_status", "sign_in_skwad", "change_initial_password"];

/// Commands that wipe or reshape everybody's data. On one machine that is
/// your own library; on a server it is hours of everyone's GPU time, so they
/// need the administrator role and leave an audit line.
const ADMIN_COMMANDS: &[&str] = &[
    "clear_scanned_data",
    "clear_selected_scanned_data",
    "clear_all_embeddings",
    "clear_all_recognition_data",
    "clear_thumbnail_cache",
    "clear_log",
    "reembed_stale_faces",
    "update_settings",
];

/// Opens the database, starts the workers, and returns the assembled state.
///
/// Separate from [`serve`] so a test can drive the API without binding a port.
pub fn boot(config: ServerConfig) -> anyhow::Result<(Arc<ServerState>, WorkerPool)> {
    let paths = AppPaths::create_with_cache(&config.library_root, config.cache_root.as_deref())?;
    let mut db_config = match &config.database_url {
        Some(url) => PgConfig::from_url(url)?,
        None => PgConfig::resolve(&paths.root),
    };
    if db_config.password.is_none() {
        // A service has no keyring entry; the pgpass file and
        // SKWAD_DATABASE_PASSWORD are what `resolve` already consults.
        tracing::debug!("no database password supplied; relying on the password file");
    }
    if let Some(workers) = config.ai_workers {
        tracing::info!(workers, "AI worker count from configuration");
    }
    db_config.max_connections = db_config.max_connections.max(8);
    tracing::info!(database = %db_config.describe(), library = %paths.root.display(), "opening the library");
    let db = Database::connect(db_config)?;
    boot_with(config, paths, db)
}

/// [`boot`] with the database already open — what a test does with a private
/// schema on the test server.
pub fn boot_with(config: ServerConfig, paths: AppPaths, db: Database) -> anyhow::Result<(Arc<ServerState>, WorkerPool)> {
    let machine_settings_file = config
        .machine_settings_file
        .clone()
        .unwrap_or_else(|| skwad_app_core::settings::machine_settings_path(&config::default_library_root().join("machine")));
    let mut settings = AppSettings::load(&db, &machine_settings_file).unwrap_or_default();
    if let Some(workers) = config.ai_workers {
        settings.ai_workers = workers;
    }
    let settings = settings.sanitised();
    let machine_id = skwad_app_core::machine::load_or_create(
        machine_settings_file
            .parent()
            .unwrap_or(&config.library_root),
    );

    // The sink needs the broadcast channel that lives on ServerState, and the
    // state needs the core, so the channel is created first and shared.
    let (events, _) = tokio::sync::broadcast::channel(sse::EVENT_BUFFER);
    let sink: Arc<dyn ProgressSink> = Arc::new(sse::SseProgressSink::new(events.clone()));

    let core = Arc::new(AppState::new(
        db,
        paths,
        settings,
        MEDIA_URL_BASE.to_string(),
        machine_id,
        machine_settings_file,
    ));
    // A seeded shared password is a defensible testing posture for a local app
    // and not for a service listening on the LAN.
    core.set_enforce_password_change(true);
    api::catalogue::ensure_local_auth(&core);

    if config.media_roots.is_empty() {
        tracing::warn!(
            "no SKWAD_SERVER_MEDIA_ROOTS configured: the folder browser is disabled, so clients \
             cannot pick a shoot source. Set it to the folders shoots live under."
        );
    }

    let auth_dir = core.paths.root.join("auth");
    let sessions = SessionStore::open(&auth_dir, Duration::from_secs(config.session_ttl_hours * 3600));

    // Workers start immediately so an import interrupted by a previous run
    // resumes without anyone having to ask.
    let workers = WorkerPool::start(Arc::clone(&sink), Arc::clone(&core));

    Ok((
        Arc::new(ServerState {
            core,
            config,
            sessions,
            sink,
            events,
            auth_dir,
            started_at: std::time::Instant::now(),
        }),
        workers,
    ))
}

// --- middleware ---------------------------------------------------------------

/// Looks up the session a request presents, if any, and attaches it. Never
/// rejects on its own: public routes want to know who is asking too.
async fn attach_session(State(state): State<Arc<ServerState>>, mut request: Request, next: Next) -> Response {
    let session: Option<SessionRecord> = auth::presented_token(&request).and_then(|token| state.sessions.resolve(&token));
    request.extensions_mut().insert(session);
    next.run(request).await
}

/// Rejects requests without a live session.
async fn require_session(request: Request, next: Next) -> Response {
    match request.extensions().get::<Option<SessionRecord>>() {
        Some(Some(_)) => next.run(request).await,
        _ => ApiError::unauthorized("sign in first").into_response(),
    }
}

/// Every `/api` caller says which contract it speaks. A mismatch is a clear
/// `426` with a human message rather than a confusing 404 or 422 later.
async fn require_api_version(request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get("x-skwad-api")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u32>().ok());
    match presented {
        Some(version) if version == config::API_VERSION => next.run(request).await,
        Some(version) => ApiError::new(
            StatusCode::UPGRADE_REQUIRED,
            format!(
                "this client speaks SKWAD API version {version}; this server speaks {}. Update the older one.",
                config::API_VERSION
            ),
        )
        .into_response(),
        None => ApiError::new(
            StatusCode::UPGRADE_REQUIRED,
            format!(
                "send X-Skwad-Api: {} — this server only answers clients that say which API version they speak",
                config::API_VERSION
            ),
        )
        .into_response(),
    }
}

// --- the command surface --------------------------------------------------------

/// The context for one request.
fn ctx_for(state: &ServerState, session: Option<&SessionRecord>, email_hint: Option<&str>) -> Ctx {
    Ctx {
        state: Arc::clone(&state.core),
        sink: Arc::clone(&state.sink),
        session: Session {
            user: session.map(SessionRecord::user),
            scope: session.map(|s| s.account_id.clone()),
        },
        identity: auth::store_for_request(&state.auth_dir, session, email_hint),
    }
}

/// `POST /api/invoke/{command}` — the whole command surface.
async fn invoke(
    State(state): State<Arc<ServerState>>,
    Extension(session): Extension<Option<SessionRecord>>,
    Path(command): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let args: serde_json::Value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("the request body is not JSON: {e}")))?
    };

    if session.is_none() && !PUBLIC_COMMANDS.contains(&command.as_str()) {
        return Err(ApiError::unauthorized("sign in first"));
    }
    if !api::COMMANDS.contains(&command.as_str()) {
        return Err(ApiError::not_found(format!("unknown command `{command}`")));
    }

    // A sign-in has no session yet; its identity store is bound to the
    // account it is signing in.
    let email_hint = args.get("email").and_then(|v| v.as_str()).map(str::to_string);
    let ctx = ctx_for(&state, session.as_ref(), email_hint.as_deref());

    if ADMIN_COMMANDS.contains(&command.as_str()) {
        let check = ctx.clone();
        let is_admin = blocking(move || Ok(api::catalogue::session_is_admin(&check)?)).await?;
        if !is_admin {
            return Err(ApiError::forbidden(
                "only an administrator can run this on a shared library",
            ));
        }
        let who = session.as_ref().map(|s| s.email.clone()).unwrap_or_default();
        let audit = Arc::clone(&state.core);
        let detail = format!("{who} ran {command}");
        let _ = blocking(move || {
            let mut conn = audit.db.conn()?;
            skwad_database::repo::logs::record_quiet(
                &mut conn,
                skwad_database::repo::logs::EVENT_ADMIN_ACTION,
                None,
                None,
                None,
                Some(&detail),
            );
            Ok(())
        })
        .await;
    }

    let name = command.clone();
    let dispatch_ctx = ctx.clone();
    let result = blocking(move || Ok(api::dispatch(&dispatch_ctx, &name, args)?)).await?;

    // Session lifecycle rides on the commands that already exist, so the
    // frontend's sign-in flow does not change.
    let mut response_headers = Vec::new();
    match command.as_str() {
        "sign_in_skwad" | "change_initial_password" => {
            let signed_in = result.get("authenticatedOnce").and_then(|v| v.as_bool()) == Some(true);
            if signed_in {
                let identity = blocking(move || Ok(ctx.identity.load()?)).await?;
                let record = state.sessions.create(skwad_app_core::api::SessionUser {
                    account_id: identity.account_id,
                    email: identity.email,
                    display_name: identity.display_name,
                });
                response_headers.push((
                    header::SET_COOKIE,
                    auth::session_cookie(
                        &record.token,
                        Duration::from_secs(state.config.session_ttl_hours * 3600),
                        state.config.tls_enabled(),
                    ),
                ));
                response_headers.push((
                    header::HeaderName::from_static(auth::SESSION_HEADER),
                    record.token.clone(),
                ));
            }
        }
        "sign_out_skwad" | "clear_authenticated_session" => {
            if let Some(session) = &session {
                state.sessions.revoke(&session.token);
            }
            response_headers.push((header::SET_COOKIE, auth::clear_cookie()));
        }
        _ => {}
    }

    let mut response = Json(result).into_response();
    for (name, value) in response_headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().append(name, value);
        }
    }
    Ok(response)
}

/// `GET /api/session` — who the presented token belongs to, for a client
/// that kept a token and wants to know whether it still works.
async fn whoami(Extension(session): Extension<Option<SessionRecord>>) -> ApiResult<Json<serde_json::Value>> {
    match session {
        Some(record) => Ok(Json(serde_json::json!({
            "accountId": record.account_id,
            "email": record.email,
            "displayName": record.display_name,
        }))),
        None => Err(ApiError::unauthorized("sign in first")),
    }
}

// --- assembly ----------------------------------------------------------------

/// Origins allowed to call the API from a page this server did not serve.
///
/// The desktop client is the reason this exists: its webview is
/// `tauri.localhost` while the server is elsewhere, so every request is
/// cross-origin. The Vite dev server is here for the same reason. Anything
/// else has to be configured — a wildcard is not an option once credentials
/// are involved, and should not be one anyway.
fn allowed_origins(config: &ServerConfig) -> Vec<HeaderValue> {
    let mut origins = vec![
        "http://tauri.localhost".to_string(),
        "https://tauri.localhost".to_string(),
        "tauri://localhost".to_string(),
        "http://localhost:1420".to_string(),
        "http://127.0.0.1:1420".to_string(),
    ];
    origins.extend(config.allowed_origins.iter().cloned());
    origins.iter().filter_map(|origin| origin.parse().ok()).collect()
}

/// The full application.
pub fn router(state: Arc<ServerState>) -> Router {
    // Everything a signed-in caller may reach besides commands.
    let session_routes = Router::new()
        .route("/api/session", get(whoami))
        .route("/api/events", get(sse::stream))
        .route("/api/fs/roots", get(fsbrowse::roots))
        .route("/api/fs/list", get(fsbrowse::list))
        .route_layer(middleware::from_fn(require_session));

    // `invoke` is what a signed-out client calls to sign in, so it cannot sit
    // behind `require_session`; the handler checks the public list itself.
    let api = Router::new()
        .route("/api/invoke/{command}", post(invoke))
        .merge(session_routes)
        .route_layer(middleware::from_fn(require_api_version));

    let media = Router::new()
        .route("/media/{kind}/{id}", get(mediaroutes::serve))
        .route_layer(middleware::from_fn(require_session));

    let mut app = Router::new()
        .route("/health", get(health::health))
        .merge(media)
        .merge(api)
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(Arc::clone(&state), attach_session));

    if let Some(dir) = state.config.web_dir.clone().filter(|dir| dir.is_dir()) {
        let index = dir.join("index.html");
        // A single-page app: unknown paths fall back to index.html so a
        // reload on a client-side route still works.
        app = app.fallback_service(
            tower_http::services::ServeDir::new(dir).fallback(tower_http::services::ServeFile::new(index)),
        );
    }

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins(&state.config)))
        .allow_credentials(true)
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-skwad-api"),
        ])
        .expose_headers([header::HeaderName::from_static(auth::SESSION_HEADER)])
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::OPTIONS]);

    app.layer(cors).layer(TraceLayer::new_for_http()).with_state(state)
}

/// Binds and serves until `shutdown` completes. `on_listening` fires once the
/// socket is accepting — the Windows service reports `Running` from it.
pub async fn serve(state: Arc<ServerState>, on_listening: impl FnOnce() + Send + 'static, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> anyhow::Result<()> {
    let bind = state.config.bind;
    let tls = match (&state.config.tls_cert, &state.config.tls_key) {
        (Some(cert), Some(key)) => Some(axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?),
        _ => None,
    };
    let app = router(Arc::clone(&state));
    let handle = axum_server::Handle::new();

    let stopper = handle.clone();
    tokio::spawn(async move {
        shutdown.await;
        tracing::info!("shutting down");
        stopper.graceful_shutdown(Some(Duration::from_secs(10)));
    });

    let waiter = handle.clone();
    tokio::spawn(async move {
        if let Some(address) = waiter.listening().await {
            tracing::info!(%address, tls = tls_label(state.config.tls_enabled()), "listening");
            on_listening();
        }
    });

    match tls {
        Some(config) => {
            axum_server::bind_rustls(bind, config)
                .handle(handle)
                .serve(app.into_make_service())
                .await?
        }
        None => {
            if !bind.ip().is_loopback() {
                tracing::warn!(
                    "listening on {bind} without TLS: sessions and media travel in the clear. Set \
                     SKWAD_SERVER_TLS_CERT and SKWAD_SERVER_TLS_KEY for anything beyond this machine."
                );
            }
            axum_server::bind(bind).handle(handle).serve(app.into_make_service()).await?
        }
    }
    Ok(())
}

fn tls_label(enabled: bool) -> &'static str {
    if enabled {
        "on"
    } else {
        "off"
    }
}

/// Runs the whole server on its own runtime, for `main` and the service.
pub fn serve_blocking(
    config: ServerConfig,
    on_listening: impl FnOnce() + Send + 'static,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let (state, workers) = boot(config)?;
        let health = health::summarise(&state.core, 0, true);
        tracing::info!("\n{}", health::render_text(&health));
        let result = serve(Arc::clone(&state), on_listening, shutdown).await;
        state.core.begin_shutdown();
        workers.join();
        result
    })
}

/// Parses `--key value` pairs into the same keys the config file uses.
pub fn cli_overrides(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        let key = match flag.as_str() {
            "--bind" => "SKWAD_SERVER_BIND",
            "--library" => "SKWAD_SERVER_LIBRARY",
            "--cache" => "SKWAD_SERVER_CACHE",
            "--database-url" => "SKWAD_DATABASE_URL",
            "--media-roots" => "SKWAD_SERVER_MEDIA_ROOTS",
            "--tls-cert" => "SKWAD_SERVER_TLS_CERT",
            "--tls-key" => "SKWAD_SERVER_TLS_KEY",
            "--allowed-origins" => "SKWAD_SERVER_ALLOWED_ORIGINS",
            "--web-dir" => "SKWAD_SERVER_WEB_DIR",
            "--machine-settings" => "SKWAD_SERVER_MACHINE_SETTINGS",
            "--session-ttl-hours" => "SKWAD_SERVER_SESSION_TTL_HOURS",
            "--ai-workers" => "SKWAD_SERVER_AI_WORKERS",
            other => return Err(format!("unrecognised flag {other}")),
        };
        let value = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        out.insert(key.to_string(), value.clone());
        i += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_webview_origin_is_allowed() {
        let origins: Vec<String> = allowed_origins(&ServerConfig::default())
            .iter()
            .map(|value| value.to_str().unwrap_or_default().to_string())
            .collect();
        assert!(origins.contains(&"http://tauri.localhost".to_string()));
        assert!(origins.contains(&"http://localhost:1420".to_string()));
        assert!(!origins.iter().any(|o| o == "*"));
    }

    #[test]
    fn flags_map_onto_config_keys() {
        let parsed = cli_overrides(&["--bind".into(), "0.0.0.0:9000".into(), "--ai-workers".into(), "2".into()]).unwrap();
        assert_eq!(parsed["SKWAD_SERVER_BIND"], "0.0.0.0:9000");
        assert_eq!(parsed["SKWAD_SERVER_AI_WORKERS"], "2");
        assert!(cli_overrides(&["--bind".into()]).is_err());
        assert!(cli_overrides(&["--nope".into(), "x".into()]).is_err());
    }

    #[test]
    fn admin_commands_exist_in_the_registry() {
        for name in ADMIN_COMMANDS.iter().chain(PUBLIC_COMMANDS) {
            assert!(api::COMMANDS.contains(name), "{name} is not a registered command");
        }
    }
}
