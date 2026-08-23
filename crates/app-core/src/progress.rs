//! How the core tells a front end that something happened.
//!
//! The core has no idea what it is attached to. A Tauri window, an SSE stream
//! to a browser, or nothing at all in a test — each supplies a [`ProgressSink`]
//! and the workers, stages and export runner push through it. This is the one
//! seam that used to be `AppHandle::emit`, and keeping it this narrow is what
//! makes a headless binary possible without a second copy of the pipeline.

use std::sync::Arc;

/// A destination for events pushed out of the core.
///
/// Implementations must never block for long and must never panic: a front end
/// that has gone away has to be irrelevant to background work that is already
/// running.
pub trait ProgressSink: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: serde_json::Value);
}

/// Discards everything. The default for tests, and for a core running before
/// any front end has attached.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullProgressSink;

impl ProgressSink for NullProgressSink {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

pub fn null_sink() -> Arc<dyn ProgressSink> {
    Arc::new(NullProgressSink)
}

/// Records what it was sent, so a test can assert on the sequence of events a
/// stage produced.
#[derive(Debug, Default)]
pub struct RecordingProgressSink {
    events: parking_lot::Mutex<Vec<(String, serde_json::Value)>>,
}

impl RecordingProgressSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<(String, serde_json::Value)> {
        self.events.lock().clone()
    }

    /// Names of the events seen so far, in order.
    pub fn names(&self) -> Vec<String> {
        self.events.lock().iter().map(|(name, _)| name.clone()).collect()
    }

    pub fn count(&self, event: &str) -> usize {
        self.events.lock().iter().filter(|(name, _)| name == event).count()
    }
}

impl ProgressSink for RecordingProgressSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        self.events.lock().push((event.to_string(), payload));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_null_sink_swallows_everything() {
        let sink = null_sink();
        sink.emit("teo://progress", serde_json::json!({ "shootId": 1 }));
    }

    #[test]
    fn the_recording_sink_keeps_order() {
        let sink = RecordingProgressSink::new();
        sink.emit("a", serde_json::Value::Null);
        sink.emit("b", serde_json::json!(2));
        sink.emit("a", serde_json::Value::Null);

        assert_eq!(sink.names(), vec!["a", "b", "a"]);
        assert_eq!(sink.count("a"), 2);
        assert_eq!(sink.events()[1].1, serde_json::json!(2));
    }
}
