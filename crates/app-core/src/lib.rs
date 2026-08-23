//! The application, minus its front door.
//!
//! Everything here used to live in `apps/desktop/src-tauri/src/`, which meant a
//! headless build would have had to duplicate it. Nothing in this crate knows
//! about Tauri, HTTP, or a window: it takes an [`AppPaths`] rather than asking a
//! path resolver, and pushes events into a [`ProgressSink`] rather than emitting
//! to a webview.
//!
//! ```text
//!            ┌─────────────────────┐        ┌──────────────────┐
//!  Tauri ───▶│                     │        │ TauriProgressSink│
//!  commands  │      teo-app-core   │──emit─▶│ SseProgressSink  │
//!  HTTP  ───▶│  state · settings   │        │ NullProgressSink │
//!  routes    │  worker · pipeline  │        └──────────────────┘
//!            │  stages · export    │
//!            └─────────────────────┘
//! ```
//!
//! The resource policy the workers implement — two workers at most, one model
//! pair owned by worker 0, inference threads capped, lazy load and idle unload —
//! is deliberate and documented in [`worker`]. It is not a tuning knob.

pub mod events;
pub mod export;
pub mod media;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod progress;
pub mod settings;
pub mod stages;
pub mod state;
pub mod worker;

pub use paths::AppPaths;
pub use progress::{null_sink, NullProgressSink, ProgressSink, RecordingProgressSink};
pub use settings::AppSettings;
pub use state::AppState;
pub use worker::WorkerPool;
