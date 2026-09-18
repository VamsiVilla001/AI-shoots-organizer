//! What every handler can reach.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use skwad_app_core::{AppState, ProgressSink};
use tokio::sync::broadcast;

use crate::auth::SessionStore;
use crate::config::ServerConfig;
use crate::sse::ServerEvent;

pub struct ServerState {
    pub core: Arc<AppState>,
    pub config: ServerConfig,
    pub sessions: SessionStore,
    /// The sink the core pushes into, shared with the workers.
    pub sink: Arc<dyn ProgressSink>,
    pub events: broadcast::Sender<ServerEvent>,
    /// `<library>/auth` — credentials file, sessions, per-account identities.
    pub auth_dir: PathBuf,
    pub started_at: Instant,
}
