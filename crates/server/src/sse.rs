//! `GET /api/events` — the HTTP transport for the core's event stream.
//!
//! Same event names and payload shapes the Tauri front end already listens
//! for (`skwad://progress`, `skwad://shoot-changed`, …), so the React side
//! switches transport without touching a single handler.
//!
//! Progress is coalesced per shoot: at most one `skwad://progress` every
//! [`PROGRESS_INTERVAL`], with terminal events always immediate. Safe by
//! construction because every payload is a snapshot rather than a delta the
//! client accumulates — dropping one loses nothing the next one does not say.
//! The stranded verification logged 37 events for three photos; this is the
//! fix.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use parking_lot::Mutex;
use skwad_app_core::{events, ProgressSink};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::state::ServerState;

pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// How many events a slow subscriber may fall behind before it starts
/// missing some. Every payload is a snapshot, so missing one is harmless.
pub const EVENT_BUFFER: usize = 256;

#[derive(Debug, Clone)]
pub struct ServerEvent {
    pub name: String,
    pub payload: serde_json::Value,
}

/// The core's sink, wired to a broadcast channel.
///
/// `send` failing means nobody is listening, which is normal — a shoot can
/// process happily with no client connected — so it is deliberately ignored.
pub struct SseProgressSink {
    events: broadcast::Sender<ServerEvent>,
    last_progress: Mutex<HashMap<i64, Instant>>,
}

impl SseProgressSink {
    pub fn new(events: broadcast::Sender<ServerEvent>) -> Self {
        Self {
            events,
            last_progress: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a progress event for this shoot should go out now.
    fn admit_progress(&self, payload: &serde_json::Value) -> bool {
        let Some(shoot_id) = payload.get("shootId").and_then(|v| v.as_i64()) else {
            return true;
        };
        let terminal = matches!(
            payload.get("stage").and_then(|v| v.as_str()),
            Some("complete") | Some("idle")
        );
        let mut last = self.last_progress.lock();
        if terminal {
            last.remove(&shoot_id);
            return true;
        }
        let now = Instant::now();
        match last.get(&shoot_id) {
            Some(at) if now.duration_since(*at) < PROGRESS_INTERVAL => false,
            _ => {
                last.insert(shoot_id, now);
                true
            }
        }
    }
}

impl ProgressSink for SseProgressSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if event == events::PROGRESS && !self.admit_progress(&payload) {
            return;
        }
        let _ = self.events.send(ServerEvent {
            name: event.to_string(),
            payload,
        });
    }
}

pub async fn stream(State(state): State<Arc<ServerState>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.events.subscribe();

    let stream = BroadcastStream::new(receiver).filter_map(|item| {
        // A lagging client has missed events it can never catch up on;
        // dropping the gap is right, because every payload is a snapshot.
        let ServerEvent { name, payload } = item.ok()?;
        Some(Ok(Event::default().event(name).data(payload.to_string())))
    });

    // The keep-alive comment is what stops an idle proxy from closing the
    // connection during a long analysis run with nothing to report.
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_sink_fans_out_with_names_intact() {
        let (tx, mut rx) = broadcast::channel(8);
        let sink = SseProgressSink::new(tx);
        sink.emit(events::SHOOT_CHANGED, serde_json::json!({ "shootId": 7, "reason": "created" }));
        let event = rx.recv().await.unwrap();
        assert_eq!(event.name, events::SHOOT_CHANGED);
        assert_eq!(event.payload["shootId"], 7);
    }

    #[test]
    fn progress_is_coalesced_per_shoot_but_terminal_events_pass() {
        let (tx, mut rx) = broadcast::channel(64);
        let sink = SseProgressSink::new(tx);
        let progress = |shoot: i64, stage: &str| serde_json::json!({ "shootId": shoot, "stage": stage, "percent": 1 });

        sink.emit(events::PROGRESS, progress(1, "analysing"));
        sink.emit(events::PROGRESS, progress(1, "analysing"));
        sink.emit(events::PROGRESS, progress(1, "analysing"));
        sink.emit(events::PROGRESS, progress(2, "analysing"));
        sink.emit(events::PROGRESS, progress(1, "complete"));

        let mut delivered = Vec::new();
        while let Ok(event) = rx.try_recv() {
            delivered.push((event.payload["shootId"].as_i64().unwrap(), event.payload["stage"].as_str().unwrap().to_string()));
        }
        assert_eq!(
            delivered,
            vec![(1, "analysing".into()), (2, "analysing".into()), (1, "complete".into())]
        );
    }

    #[test]
    fn emitting_with_nobody_listening_is_not_an_error() {
        let (tx, rx) = broadcast::channel(8);
        drop(rx);
        let sink = SseProgressSink::new(tx);
        sink.emit(events::NOTICE, serde_json::json!({ "level": "info", "message": "hello" }));
    }
}
