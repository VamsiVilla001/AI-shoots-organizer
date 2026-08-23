//! What every handler is given.

use std::sync::Arc;

use teo_app_core::AppState;
use tokio::sync::broadcast;

use crate::config::ServerConfig;

/// One event on its way to every connected browser.
#[derive(Debug, Clone)]
pub struct ServerEvent {
    pub name: String,
    pub payload: serde_json::Value,
}

/// Events are fanned out through a broadcast channel: late subscribers do not
/// replay history, and a slow client is dropped from the tail rather than
/// stalling the worker that emitted.
pub const EVENT_BUFFER: usize = 256;

pub struct ServerState {
    /// The application core — the same one the desktop app runs.
    pub core: Arc<AppState>,
    pub config: ServerConfig,
    pub token: String,
    pub events: broadcast::Sender<ServerEvent>,
}

impl ServerState {
    pub fn new(core: Arc<AppState>, config: ServerConfig, token: String) -> Arc<Self> {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Arc::new(Self { core, config, token, events })
    }
}
