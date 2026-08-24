# Server edition — running design note

**Goal:** one core, two front doors. The desktop app keeps its current
behaviour and performance; a NAS-hosted server edition becomes possible without
forking the codebase.

This is a working document, updated at the end of each phase. It becomes the
source for the final product documentation. For how the work actually went —
decisions, bugs found, what was verified against real data — see
[work-log-2026-08.md](work-log-2026-08.md).

## Status

| Phase | State |
| --- | --- |
| 0 — groundwork | done |
| 1 — extract the Tauri-free core | done |
| 2 — HTTP server | done |
| 3 — one front end, two transports | done |
| 4 — desktop shell becomes a client | done |
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
        │  paths    events  media  │
        └──────────┬───────────────┘
                   │
   database · media-core · face-detection · face-recognition
   clustering · video-analysis · export-engine
```

Crates added:

- **`crates/app-core` (`teo-app-core`)** — the application minus its front door.
- **`crates/server` (`teo-server`)** — the HTTP front door: routes, SSE, media,
  filesystem browser, auth, static bundle. Configured entirely from flags and
  environment.

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

## Phase 2 — the HTTP server

`crates/server` is axum over the same core the desktop app runs: same database,
same job queue, same worker policy. `boot()` opens the database, starts the
worker pool and returns the state; `serve()` binds and runs the router.

Configuration is flags first, environment second: `TEO_BIND`, `TEO_DATA_DIR`,
`TEO_MEDIA_ROOTS`, `TEO_OUTPUT_ROOTS`, `TEO_TOKEN`, `TEO_WEB_DIR`.

### Routes

The command layer ported one for one — same names, same inputs, same outputs.
Sixty-two commands became routes under `/api`, grouped exactly as `commands.rs`
groups them. Paths use axum 0.8 syntax (`{id}`, not `:id`); the URLs a client
calls are unchanged by that.

Four deliberate deviations from the task list's table, none of them a redesign:

| Table said | Built | Why |
| --- | --- | --- |
| `POST /api/shoots/:id/pause` | `POST /api/processing/pause` | Pausing is process-wide in the core, exactly as `pause_processing` is. A per-shoot URL would be a lie. |
| `POST /api/shoots/:id/scan` | — | There is no separate scan command: `resume` re-enqueues the scan and the queue is idempotent. A second route doing the same thing invites drift. |
| `GET /api/jobs/summary` | built, but new | No desktop command matches it — the sidebar reads per-shoot progress. It aggregates `list_summaries`, adding no new database code. |
| — | `/api/groups/*` | The manual grouping commands (§34) postdate the table and need routes too. |

`reveal_in_folder` and `open_path` have no server equivalent by nature: they open
a file manager on the machine running the code, which for a NAS is not the
machine the person is sitting at.

### Media

`/media/{id}/{thumb,full,frame,stream}` replaces `teomedia://`. The lookup, the
HEIC/raw render and the `Range` arithmetic moved into `teo_app_core::media`, so
the protocol handler and the HTTP routes share one implementation rather than
drifting apart. `stream` answers `206` with `Content-Range` for a ranged request
and `200` otherwise, and ids remain the only thing either front door accepts —
no route takes a path.

`frame?t=<seconds>` renders one frame of a video on demand. A face found in a
clip is stored with the timestamp it was found at, while the cached thumbnail is
a poster frame a tenth of the way in — so cropping a face box out of the poster
frames whatever happened to be at those coordinates in a different second. The
route falls back to the poster frame when the render fails, since a tile showing
the wrong moment is what it showed before the route existed, and better than a
broken image.

### Events

`GET /api/events` is Server-Sent Events over a `tokio::sync::broadcast` channel;
`SseProgressSink` is the core sink implementation. Event names and payloads are
untouched, so Phase 3 can switch transport without touching a handler. A lagging
subscriber drops the gap rather than stalling a worker: every payload is a
snapshot, not a delta.

### Filesystem browser

`/api/fs/roots` and `/api/fs/list` replace the native folder picker. Every
incoming path is canonicalised — resolving `..` and symlinks — and confirmed to
sit inside a configured root before anything is read. A parent link is only
offered while it stays inside a root, so the picker cannot walk out one level at
a time. Listings give subdirectory names and media counts, one level deep: no
file names, no sizes, no way to enumerate a share.

Canonicalising on Windows yields `\\?\C:\…`; `config::tidy` strips that before
a path reaches a caller, because it leaks into export reports and error messages
and some tools reject it.

### Export destinations

`validate_destination` now checks identity as well as path: a container commonly
mounts one share twice — `/media` read-only, `/output` writable — and those
canonicalise differently while being the same directory. Comparing `(dev, ino)`
on Unix catches it for the destination and every ancestor of it. Windows has no
stable file index, so the path check stands alone there, as it always has on the
desktop.

On top of that the server confines destinations to `TEO_OUTPUT_ROOTS` (falling
back to the media roots). With neither configured — a loopback server behind the
desktop app, where a native picker chose the folder — any path is allowed and the
engine's own guard is the only check, which is exactly the desktop's position.

### Auth

One shared bearer token over every `/api/*` and `/media/*` route, compared in
constant time, taken from `TEO_TOKEN` or generated into `<data>/token` (`0600`
on Unix; Windows has no mode bits, so the directory is the thing to lock down).
`POST /api/auth/session` trades the token for an `HttpOnly`, `SameSite=Strict`
cookie, because `<img>` and `<video>` cannot send headers and media has to load
without signing every URL.

### Verified

Driven with `curl` against a running server, a temporary data directory and
three real photos:

- no token → `401`; wrong token → `401`; correct token → `200`
- `C:\Windows` and a `..` escape → `403`, from both the browser and shoot creation
- shoot created → `scanning` → `analysing` → `complete`, 11 faces over 3 photos
- 37 SSE events arrived while it ran: 32 `teo://progress`, 4 `teo://shoot-changed`, 1 `teo://notice`
- `/media/{id}/thumb` → `200 image/jpeg`, 27,666 bytes, valid JPEG magic
- group two files → export → `2/2 completed`, folders and `_sorting-report.txt`
  on disk, source folder unchanged
- destination inside the source → `403`
- cookie session → media loads with no `Authorization` header; wrong token → `401`
- `GET /` served the built React bundle

## Phase 3 — one front end, two transports

The same React bundle now runs inside the Tauri window and in a browser talking
to `teo-server`. Four files used to import `@tauri-apps` directly; one does now,
and that one is a transport implementation.

```text
   screens · components · api.ts · media.ts · eventBridge.ts
                            │
                    transport() — chosen once at boot
                    ┌───────┴────────┐
            TauriTransport      HttpTransport
            invoke / listen     fetch / EventSource
            native dialog       /api/fs/* browser
```

### The seam

`Transport` is five methods: `call`, `listen`, `mediaUrl`, `setMediaBase`, and an
optional `pickFolder`. Command *names* stay the contract — `api.ts` asks for
`list_shoots` and knows nothing about URLs — so `transport/routes.ts` is the
only file aware the API is HTTP at all. It mirrors the route list in
`crates/server/src/lib.rs` so the two can be diffed by eye.

Three details that mattered:

- **`nullOn404`.** A few commands return `T | null` on the desktop where the
  server answers `404` (`get_shoot`, `get_media`). The route table marks them
  rather than making every caller handle both.
- **Media URLs differ in shape**, not just in prefix: `teomedia://thumb/7`
  against `/media/7/stream`. `media.ts` asks the transport for "the thumbnail of
  this id" instead of concatenating a base.
- **`pickFolder` is optional by design.** Its absence is how `PathPicker` knows
  to open the server's jailed browser instead — a capability check, not a
  platform check.

### Desktop-only commands

`reveal_in_folder` and `open_path` throw `UnsupportedByTransport` in a browser,
with a message naming what cannot happen. They open a file manager on the
machine running the code, which for a NAS is not where the person is sitting.

### Connecting

A browser build tries a saved connection silently and only shows the connection
screen when there is none or it fails — with the previous values kept and a
message that separates "token refused" from "nothing answered", because the fix
differs. `POST /api/auth/session` runs first so the cookie is in place before any
`<img>` or `EventSource` needs it.

### Verified under both transports

Driven through a real browser against a running server: connection screen →
connected → **created a shoot using the jailed folder browser** (which showed
only the configured roots and `Day2` with its 3 media) → watched it reach
`Processing complete`, 3/3 scanned, 9 faces, with the panel updating from SSE →
sorted two files into a group → exported to `out/`, three files in two folders
plus the report, source folder unchanged. A reload reconnected silently from the
saved connection. The export picker offered only the writable root.

Then the packaged desktop build: `hasTauri` true, `mediaUrlBase`
`http://teomedia.localhost`, thumbnails loading over the protocol handler, the
existing 253-file shoot intact, and a backend event arriving through
`TauriProgressSink` as `{"reason":"albums","shootId":1}`.

## Phase 4 — the desktop shell becomes a client

`apps/desktop/src-tauri` is now 528 lines: a window, a log, a supervisor and
three commands. The duplication Phase 2 introduced deliberately is gone — the
Tauri command layer was deleted, and both editions run the one in `teo-server`.

The shell binary went from 32 MB to **6 MB**, because it no longer links the
database, the AI crates or ONNX Runtime. That weight moved to `teo-server.exe`
(27.5 MB); the installer ships both.

### How it runs

`Supervisor::start` spawns `teo-server` with `--bind 127.0.0.1:0`, a token
generated per launch and passed on the command line, and `--port-file`. Port 0
means the OS picks a free port, so two launches never fight over one; the child
writes the address it actually bound and the parent reads it, because a parent
that asked for port 0 has no other way to learn it and pre-picking one would race
with everything else on the machine. The child's stdout and stderr are forwarded
into `logs/teo.log`, so one log tells the whole story.

The token is never written to disk in this mode. The file-based token exists for
the NAS case, where a person has to read it.

A watcher thread restarts the child if it exits on its own, backing off a little
each time and giving up after five in a row — at which point the UI says so
rather than looping. `CloseRequested` kills it, because leaving it running would
hold the database and a port after the window is gone.

### What the webview loads

The **embedded** bundle, not the server's copy. A first paint never waits on a
port, and a broken static-file path cannot stop the app from starting. That makes
every request cross-origin — page on `tauri.localhost`, server on `127.0.0.1` —
which surfaced two things a same-origin browser test could never have shown:

1. **CORS.** The server now allows the Tauri webview origins and the Vite dev
   server explicitly, with credentials. A wildcard is invalid once credentials
   are involved and wrong regardless; anything else goes in
   `TEO_ALLOWED_ORIGINS`.
2. **`SameSite` cookies are never sent cross-site**, so the session cookie
   cannot authenticate `EventSource` or an `<img>`. Those carry `?token=…`
   instead, which the middleware accepts as a third option after the header and
   the cookie. Loopback only, with a token that lasts one launch — which is why
   a token in a URL is acceptable there and unused in the browser edition, where
   everything is same-origin.

### Reconnecting

A restart means a new port, so the desktop transport retries any request that
fails to *connect* once, after re-asking the shell where the server went, and
moves live event subscriptions onto the new connection. It then fires
`teo:endpoint-changed`, which Boot turns into a full query invalidation: anything
fetched through the old connection has to be fetched again.

### Upgrading from 0.1.0

The data layout is untouched — `com.teorganiser.desktop/{database, thumbnails,
face_cache, models, logs}` — because the server is pointed at the same directory
the shell used to use. An existing install opens its existing `media.db` and
continues from whatever migration it was on.

### Verified

Launched against the real 297-file library on this machine: server on a random
loopback port with a 64-character per-launch token, `Sort — Day 2` showing 297
files with 44 sorted and the `Spero` group intact, thumbnails loading over
loopback with a URL token. Then `Stop-Process` on the child: the supervisor
restarted it on a new port (restart count 1), the transport re-pointed itself,
and thumbnails came back on the new port with no error screen and no reload.

## Naming a person gathers their footage

The sequence the work actually follows: choose a photo, ask who is in it, name
one of them, and their footage lands in a folder-shaped group. Naming a second
face in the same photo does the same for that person, which is how one group
shot becomes several people's groups.

`POST /api/faces/name` is that whole loop in one call, because doing it in four
round trips leaves the library half-updated when one of them fails:

1. the name becomes a player, reused when the name is already known;
2. every face in the same unknown cluster is assigned to them — a cluster is one
   person by construction, so naming one face names them everywhere the
   clusterer found them;
3. albums are regenerated, which is what knows every file a player appears in;
4. a group named after them is created or topped up from that album.

Step 4 is the point. Naming without it leaves the editor sorting by hand anyway.

Two entry points, one operation: **Name people (N)** in the viewer and **Name
people in it** on a single selected file in the Sort screen both open the same
guided flow, and the click-to-tag popover calls the same command, so the quick
path and the guided path cannot drift apart.

Measured on a copy of a real 183-file shoot: naming one face in a five-face
photo matched 71 faces and gathered **72 files** into that person's group;
naming a second face in the same photo gathered another 20 into theirs, taking
the shoot from 0 sorted to 80 in two answers.

## Naming a face rather than a file

A photo with five people in it has five answers to "who is this?", so the
question is asked about a face. In the viewer every detected face is a clickable
box: unnamed ones are dashed and amber and say *Tap to name*, named ones carry
the name and the match confidence. Clicking opens a small tagger beside the box
— type or pick a player, and only that face is assigned.

Two details that matter in use:

- **A multi-person photo says so.** With two or more faces the viewer shows
  "5 people here, 4 not named yet — click a face to say who it is", which is the
  prompt the whole feature exists to answer. It hides while a tagger is open.
- **"Tag & add to group"** sits next to "Tag", because identifying someone is
  rarely the point on its own: the reason to know it is Jonathan is to get the
  shot into Jonathan's folder. It files the photo into the group of that name,
  creating it on first use.

`Wrong person` returns a face to the unknown pool and `Not a face` marks a false
detection, so a mis-tag is correctable in the same place it was made.

The dialog for naming an unknown group had the same ambiguity from the other
direction — it showed a cover *photo*, which with four people in it says nothing
about which one the group is. It now shows a strip of the actual face crops,
using the existing face query and the CSS-crop component, so what is being named
is visible before the name is typed.

## Decisions worth keeping

- **The sink is on the state, not threaded through every call.** It is available
  wherever the state is, which is everywhere that needs it, and it keeps the
  diff to the command layer down to the emit sites.
- **`serde_json::Value` at the boundary.** The trait cannot be generic and stay
  object-safe. Payload structs remain typed; only the last step is dynamic.
- **`teomedia://` stays in the desktop shell**, but its body moved. The Tauri
  file is now an adapter over `teo_app_core::media`; the HTTP routes are a
  second adapter. Two transports, one implementation of "id in, bytes out".
- **Handlers wrap database work in `blocking`.** The core is synchronous by
  design — pooled SQLite, rayon, std threads — so a handler that called it
  directly would hold an async runtime thread for the duration of a scan.

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
