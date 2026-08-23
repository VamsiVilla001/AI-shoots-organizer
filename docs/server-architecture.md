# Server edition — running design note

**Goal:** one core, two front doors. The desktop app keeps its current
behaviour and performance; a NAS-hosted server edition becomes possible without
forking the codebase.

This is a working document, updated at the end of each phase. It becomes the
source for the final product documentation.

## Status

| Phase | State |
| --- | --- |
| 0 — groundwork | done |
| 1 — extract the Tauri-free core | done |
| 2 — HTTP server | not started |
| 3 — one front end, two transports | not started |
| 4 — desktop shell becomes a client | not started |
| 5 — NAS container | not started |
| 6 — remote GPU worker | not started |
| 7 — multi-user hardening | not started |

## Shape

```text
        ┌──────────────────────────┐        ┌────────────────────────┐
Tauri ─▶│                          │        │  TauriProgressSink     │
command │       teo-app-core       │──emit─▶│  (SseProgressSink)     │
layer   │                          │        │  NullProgressSink      │
        │  state    settings       │        │  RecordingProgressSink │
HTTP ──▶│  worker   pipeline       │        └────────────────────────┘
routes  │  stages   export  models │
(soon)  │  paths    events         │
        └──────────┬───────────────┘
                   │
   database · media-core · face-detection · face-recognition
   clustering · video-analysis · export-engine
```

Crates added:

- **`crates/app-core` (`teo-app-core`)** — the application minus its front door.
- **`crates/server` (`teo-server`)** — lib + bin scaffold. Config only so far:
  `TEO_BIND`, `TEO_DATA_DIR`, `TEO_MEDIA_ROOTS`, `TEO_OUTPUT_ROOTS`, `TEO_TOKEN`.

Both are registered in the root `Cargo.toml` under `[workspace.members]`,
`[workspace.dependencies]` **and** `[profile.dev.package.*]` with
`opt-level = 3`. That last one is not optional: Cargo's `"*"` glob does not
cover workspace members, and without it inference and clustering compile
unoptimised during `npm run dev`.

## Phase 1 — what moved and what changed

Moved from `apps/desktop/src-tauri/src/` into `crates/app-core/src/`, with their
tests: `settings.rs`, `worker.rs`, `pipeline.rs`, `stages.rs`, `state.rs`,
`export.rs`, `events.rs`, plus `paths.rs` and `models.rs` — the last two came
along because `AppState` and the pipeline hold them, and a headless build needs
both.

Three seams were cut:

1. **Events.** `ProgressSink` replaces `AppHandle::emit`:

   ```rust
   pub trait ProgressSink: Send + Sync + 'static {
       fn emit(&self, event: &str, payload: serde_json::Value);
   }
   ```

   The sink lives on `AppState`, so the worker pool, the monitor and the export
   runner reach it through `state.sink()` instead of carrying a handle. Event
   names and payload structs are unchanged — `teo://progress`,
   `teo://shoot-changed`, `teo://library-changed`, `teo://job-failed`,
   `teo://export-progress`, `teo://notice` — because the React side listens on
   them and Phase 3 switches transport without touching handlers.
   `NullProgressSink` and `RecordingProgressSink` are the test doubles.

2. **Paths.** `AppState::new` takes an explicit `AppPaths`. Nothing in the core
   asks a path resolver where the data directory is; the desktop shell resolves
   it from Tauri and passes it in, and the server will read `TEO_DATA_DIR`.

3. **Signatures.** `WorkerPool::start(state)` and `export::start(state, …)` lost
   their `AppHandle` parameters, as did every command that only held one to emit
   with. `media_url_base` stays on `AppState` as a plain string: the desktop
   passes the `teomedia://` base, the server will pass an HTTP prefix.

The resource policy is untouched, deliberately: two workers at most, worker 0
owning the single detector/embedder pair and the shoot-wide finishing stages,
worker 1 on scan and thumbnails, inference threads capped at four, one logical
CPU reserved, lazy model load, 30-second idle unload, rebuild on
`settings_version` change, progress throttled to ~500 ms.

`apps/desktop/src-tauri` now contains only `commands.rs`, `protocol.rs`,
`sink.rs` and the app wiring in `lib.rs`. It re-exports the core's modules under
their old names (`pub use teo_app_core::{events, export, …}`) so the command
layer's `crate::state::AppState`-style paths keep working — the command bodies
are unchanged apart from where they emit.

Verified after the move: `cargo test --workspace` (56 tests: 48 in `app-core`,
8 in the desktop shell, 1 in the server scaffold), `cargo clippy --workspace
--all-targets -- -D warnings`, `npm run typecheck`, `npm run web:build`, and the
packaged desktop app launching and driving a real shoot.

## Decisions worth keeping

- **The sink is on the state, not threaded through every call.** It is available
  wherever the state is, which is everywhere that needs it, and it keeps the
  diff to the command layer down to the emit sites.
- **`serde_json::Value` at the boundary.** The trait cannot be generic and stay
  object-safe. Payload structs remain typed; only the last step is dynamic.
- **`teomedia://` stays in the desktop shell.** Phase 2 adds HTTP media routes
  rather than moving the protocol handler, because the two resolve ids the same
  way but differ entirely in transport.

## Open questions

- **Where does `commands.rs` end up?** Phase 4 deletes it once the desktop shell
  talks to a loopback server. Until then the HTTP routes are a parallel
  implementation over the same core, which is the point of the one-to-one port
  in Phase 2 — two front doors that are trivially comparable.
- **Auth for the desktop-as-client case.** A per-launch token held in memory
  (Phase 4.1) never touches disk; the NAS case needs the `/config/token` file.
  Both go through the same middleware.
- **`media_url_base` may not survive.** Phase 3.2 puts URL construction behind a
  helper in the front end, which may make the backend field redundant.
