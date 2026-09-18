//! Localhost-only HTTP bridge for the Premiere Pro panel.
//!
//! An external process — the Premiere Pro UXP panel — cannot reach the
//! webview's `skwadmedia://` protocol or Tauri's IPC, so it needs an ordinary
//! HTTP endpoint instead. This server exists for exactly that: it binds
//! loopback-only on a fixed port and exposes read-only Collection data so the
//! panel can build a Premiere bin that references the original files —
//! nothing is copied or written here.
//!
//! Auth is a fixed shared token baked into both this file and the panel's
//! `main.js`, not a per-install secret. That's a deliberate choice, not an
//! oversight: this only ever listens on loopback (unreachable from outside
//! the machine), the "who am I" question is answered by the already
//! signed-in desktop session (`current_project_identity`), not by the token —
//! so the token's only job is telling the bridge apart from an unrelated
//! local process, which a fixed constant does exactly as well as a random
//! per-install one would. In exchange, the panel never needs a setup step:
//! no discovery file to read, nothing to paste.

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

/// Must match `BRIDGE_TOKEN` in apps/premiere-panel/main.js exactly.
const BRIDGE_TOKEN: &str = "skwad-premiere-bridge-v1";

/// One node in a Project's Collection tree — `parentId`/`sortOrder` are
/// exactly what the app's own Collections screen uses to build folders and
/// order them (`nestedCollections.tsx`'s `childrenOf`), so the panel can
/// reproduce the same nested structure instead of a flat list.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionNode {
    id: String,
    name: String,
    parent_id: Option<String>,
    sort_order: i64,
    media_count: usize,
}

/// A Project the signed-in user can reach, with its full Collection tree.
/// `accessRole`/`visibility`/`status`/`ownerEmail` are exactly what the app's
/// Collections screen uses to sort projects into Personal / Shared with me /
/// Organisation / Archived (`nestedCollections.tsx`'s `inView`) and to label
/// them (`accessLabel`) — mirrored here so the panel can do the same.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectSummary {
    id: String,
    name: String,
    owner_email: String,
    access_role: String,
    visibility: String,
    status: String,
    collections: Vec<CollectionNode>,
}

/// `email` lets the panel show "Signed in as …" without needing to guess it
/// from `accessRole == owner` on a project that might not exist yet.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectsResponse {
    email: String,
    projects: Vec<ProjectSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionFile {
    path: String,
    filename: String,
    is_video: bool,
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

impl From<skwad_app_core::api::ApiError> for ApiError {
    fn from(e: skwad_app_core::api::ApiError) -> Self {
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
    tracing::info!(port = BRIDGE_PORT, "Premiere bridge listening");

    // Unauthenticated on purpose: a plain "is anything listening on this port"
    // check the panel (or a person with a browser) can make before it has a
    // token at all.
    let protected = Router::new()
        .route("/projects", get(list_projects))
        .route("/collections/{id}/media", get(collection_media))
        .route("/pending", get(pending_jobs))
        .route_layer(middleware::from_fn(require_token));

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

async fn health() -> &'static str {
    "ok"
}

async fn require_token(request: Request, next: Next) -> Response {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if provided != Some(BRIDGE_TOKEN) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

/// Every Project the signed-in user can reach, each with its full Collection
/// tree — same access rule as the `list_projects` Tauri command, and the same
/// shape of data the app's own Collections screen navigates.
async fn list_projects(State(state): State<Arc<AppState>>) -> Result<Json<ProjectsResponse>, ApiError> {
    let (account_id, email, organisation) = skwad_app_core::api::catalogue::current_project_identity(&crate::commands::headless_ctx(Arc::clone(&state)))?;
    let mut conn = state.db.conn()?;
    let accessible = projects::list_accessible(&mut conn, &account_id, &email, organisation.as_deref())?;
    drop(conn);

    let mut out = Vec::new();
    for project in accessible {
        let mut collections = Vec::with_capacity(project.collections.len());
        for collection in &project.collections {
            let media_count = export::resolve_collection_files(&state.db, &collection.sources)?.len();
            collections.push(CollectionNode {
                id: collection.id.clone(),
                name: collection.name.clone(),
                parent_id: collection.parent_id.clone(),
                sort_order: collection.sort_order,
                media_count,
            });
        }
        out.push(ProjectSummary {
            id: project.id,
            name: project.name,
            owner_email: project.owner_email,
            access_role: project.access_role,
            visibility: project.visibility,
            status: project.status,
            collections,
        });
    }
    Ok(Json(ProjectsResponse { email, projects: out }))
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
    let (account_id, email, organisation) = skwad_app_core::api::catalogue::current_project_identity(&crate::commands::headless_ctx(Arc::clone(&state)))?;
    let mut conn = state.db.conn()?;
    let accessible = projects::list_accessible(&mut conn, &account_id, &email, organisation.as_deref())?;
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
