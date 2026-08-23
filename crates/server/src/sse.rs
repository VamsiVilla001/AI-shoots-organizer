//! `GET /api/events` — the HTTP transport for the core's event stream.
//!
//! Same event names and payload shapes the Tauri front end already listens for
//! (`teo://progress`, `teo://shoot-changed`, …), so the React side can switch
//! transport without touching a single handler.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use teo_app_core::ProgressSink;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::state::{ServerEvent, ServerState};

/// The core's sink, wired to a broadcast channel.
///
/// `send` failing means nobody is listening, which is normal — a shoot can
/// process happily with no browser open — so it is deliberately ignored.
pub struct SseProgressSink {
    events: broadcast::Sender<ServerEvent>,
}

impl SseProgressSink {
    pub fn new(events: broadcast::Sender<ServerEvent>) -> Self {
        Self { events }
    }
}

impl ProgressSink for SseProgressSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.events.send(ServerEvent { name: event.to_string(), payload });
    }
}

pub async fn stream(
    State(state): State<Arc<ServerState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.events.subscribe();

    let stream = BroadcastStream::new(receiver).filter_map(|item| {
        // A lagging client has missed events it can never catch up on; dropping
        // the gap is right, because every payload is a snapshot rather than a
        // delta the client accumulates.
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
    async fn the_sink_fans_out_to_subscribers_with_names_intact() {
        let (tx, mut rx) = broadcast::channel(8);
        let sink = SseProgressSink::new(tx);

        sink.emit("teo://progress", serde_json::json!({ "shootId": 7, "percent": 12.5 }));

        let event = rx.recv().await.unwrap();
        assert_eq!(event.name, "teo://progress");
        assert_eq!(event.payload["shootId"], 7);
    }

    #[tokio::test]
    async fn emitting_with_nobody_listening_is_not_an_error() {
        let (tx, rx) = broadcast::channel(8);
        drop(rx);
        let sink = SseProgressSink::new(tx);
        // Would panic or block if the implementation cared.
        sink.emit("teo://notice", serde_json::json!({ "level": "info", "message": "hello" }));
    }
}
