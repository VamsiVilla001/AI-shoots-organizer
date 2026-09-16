//! Localhost-only HTTP bridge for the Premiere Pro panel.
//!
//! An external process — the Premiere Pro UXP panel — cannot reach the
//! webview's `skwadmedia://` protocol or Tauri's IPC, so it needs an ordinary
//! HTTP endpoint instead. This server exists for exactly that: it binds
//! loopback-only on a fixed port, requires a per-launch bearer token
//! (discovered via `premiere-bridge.json` in the app data directory, which
//! the panel and this process both have access to on disk), and exposes
//! read-only Collection data so the panel can build a Premiere bin that
//! references the original files — nothing is copied or written here.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path as RoutePath, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use skwad_database::repo::projects;
use tower_http::cors::CorsLayer;

use crate::export;
use crate::state::{AppState, PremiereJob};

/// Fixed rather than OS-assigned: the UXP panel's manifest must allow-list an
/// exact `domain:port` (its network sandbox doesn't reliably support a
/// wildcard port), so the port has to be known ahead of time. If this port is
/// already taken — most likely another instance of this app still shutting
/// down — the bridge simply doesn't start; nothing else in the app depends on
/// it.
const BRIDGE_PORT: u16 = 51823;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionSummary {
    id: String,
    name: String,
    project_id: String,
    project_name: String,
    media_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionFile {
    path: String,
    filename: String,
    is_video: bool,
}

#[derive(Serialize)]
struct Discovery {
    port: u16,
    token: String,
}

enum ApiError {
    NotFound,
    Internal(String),
}

impl From<skwad_database::DbError> for ApiError {
    fn from(e: skwad_database::DbError) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<export::ExportRunError> for ApiError {
    fn from(e: export::ExportRunError) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<crate::commands::CommandError> for ApiError {
    fn from(e: crate::commands::CommandError) -> Self {
        ApiError::Internal(e.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "collection not found").into_response(),
            ApiError::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message).into_response(),
        }
    }
}

/// Starts the bridge on a background thread with its own single-threaded
/// tokio runtime — the rest of the app has no async runtime today, and one
/// listener doesn't warrant giving the whole desktop process one.
pub fn start(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("skwad-premiere-bridge".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    tracing::error!(error = %e, "could not start the Premiere bridge runtime");
                    return;
                }
            };
            runtime.block_on(serve(state));
        })
        .expect("failed to spawn the Premiere bridge thread");
}

async fn serve(state: Arc<AppState>) {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", BRIDGE_PORT)).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(error = %e, port = BRIDGE_PORT, "could not bind the Premiere bridge port");
            return;
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!(error = %e, "could not read the Premiere bridge port");
            return;
        }
    };

    if let Err(e) = write_discovery_file(&state, addr) {
        tracing::error!(error = %e, "could not write the Premiere bridge discovery file");
        return;
    }
    tracing::info!(port = addr.port(), "Premiere bridge listening");

    // Unauthenticated on purpose: a plain "is anything listening on this port"
    // check the panel (or a person with a browser) can make before it has a
    // token at all.
    let protected = Router::new()
        .route("/collections", get(list_collections))
        .route("/collections/{id}/media", get(collection_media))
        .route("/pending", get(pending_jobs))
        .route_layer(middleware::from_fn_with_state(Arc::clone(&state), require_token));

    // Permissive on purpose: this only ever listens on loopback, and the
    // bearer token — not the browser's origin check — is what actually
    // gates access. Restricting origins here would just break testing the
    // bridge from a plain browser page (see test-bridge.html) for no real
    // security gain.
    let app = Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(CorsLayer::permissive())
        .with_state(state);

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "Premiere bridge stopped");
    }
}

fn write_discovery_file(state: &AppState, addr: SocketAddr) -> std::io::Result<()> {
    let discovery = Discovery {
        port: addr.port(),
        token: state.premiere_token.clone(),
    };
    std::fs::write(
        state.paths.root.join("premiere-bridge.json"),
        serde_json::to_vec_pretty(&discovery).unwrap_or_default(),
    )
}

async fn health() -> &'static str {
    "ok"
}

async fn require_token(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if provided != Some(state.premiere_token.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

/// Every `ProjectCollection` across every Project the signed-in user can
/// reach — same access rule as the `list_projects` command.
async fn list_collections(State(state): State<Arc<AppState>>) -> Result<Json<Vec<CollectionSummary>>, ApiError> {
    let (account_id, email, organisation) = crate::catalogue::current_project_identity(&state)?;
    let conn = state.db.conn()?;
    let accessible = projects::list_accessible(&conn, &account_id, &email, organisation.as_deref())?;
    drop(conn);

    let mut out = Vec::new();
    for project in accessible {
        for collection in project.collections {
            let media_count = export::resolve_collection_files(&state.db, &collection.sources)?.len();
            out.push(CollectionSummary {
                id: collection.id,
                name: collection.name,
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                media_count,
            });
        }
    }
    Ok(Json(out))
}

/// Jobs queued by a "Send to Premiere" context-menu action in this app since
/// the panel's last poll (see `state::AppState::enqueue_premiere_job`).
async fn pending_jobs(State(state): State<Arc<AppState>>) -> Json<Vec<PremiereJob>> {
    Json(state.drain_premiere_queue())
}

/// The files one Collection resolves to, for the panel to hand straight to
/// Premiere's `importFiles`.
async fn collection_media(
    State(state): State<Arc<AppState>>,
    RoutePath(collection_id): RoutePath<String>,
) -> Result<Json<Vec<CollectionFile>>, ApiError> {
    let (account_id, email, organisation) = crate::catalogue::current_project_identity(&state)?;
    let conn = state.db.conn()?;
    let accessible = projects::list_accessible(&conn, &account_id, &email, organisation.as_deref())?;
    drop(conn);

    let collection = accessible
        .into_iter()
        .flat_map(|project| project.collections)
        .find(|collection| collection.id == collection_id)
        .ok_or(ApiError::NotFound)?;

    let files = export::resolve_collection_files(&state.db, &collection.sources)?;
    Ok(Json(
        files
            .into_iter()
            .map(|file| CollectionFile {
                path: file.path.display().to_string(),
                filename: file.filename,
                is_video: file.is_video,
            })
            .collect(),
    ))
}
