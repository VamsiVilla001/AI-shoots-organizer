# Work log — August 2026

A record of one working session: what was asked, what was built, why each
load-bearing decision went the way it did, what was actually verified, and what
is knowingly unfinished. Written so the next person — or the next agent — can
pick this up without reconstructing it from diffs.

Sections 1–11 are organised by concern. **Section 12 is the conversation
itself** — the requests in order, and the two places where what was asked for and
what was first built came apart.

**Branch:** `feat/server-extraction` (14 commits, on top of three inherited from
`V1`)
**Base:** `cae09d8` "SKWAD Media Organiser: local-first player-wise shoot
sorting", v0.1.0
**Machine it ran on:** Windows 10 Pro, 16 logical cores, DirectML available,
FFmpeg on PATH, a real 297-file shoot on `\\TESS-CREATIVE-12bay`

---

## 1. Where the application started

A local-first Tauri 2 desktop app that sorted esports shoots by player:
scan a folder, detect and embed faces (SCRFD + ArcFace via ONNX Runtime),
cluster unknowns, name them, browse generated albums, and copy originals into
per-player folders. Everything in `apps/desktop/src-tauri/` — commands, state,
workers, pipeline, media protocol — plus seven `crates/` for the work itself.

Two properties were treated as inviolable throughout, and still hold:

- **Source media is never modified, moved or deleted.** Every export copies.
- **Human decisions survive regeneration.** Albums, clusters and suggestions are
  derived and rebuildable; a confirmed assignment or a named cluster is not.

## 2. What was asked, in order

| # | Request | Outcome |
| --- | --- | --- |
| 1 | Editors must group footage by whose it is, name those groups in the app, and have the names become folders in a new directory — source untouched | §3 |
| 2 | "Build the changes into app and launch it" | §4 |
| 3 | "Give me a way to deploy it for others… I need a Mac Studio version" | §5 |
| 4 | Work the server-refactor task list (`TASKS-server-refactor.md`), phases 0–4 | §6 |
| 5 | Naming a photo with several people must ask *which* person, by clicking them | §7 |

---

## 3. Manual grouping — the feature the app was missing

**The gap.** The AI half already worked. What did not exist was the editor's own
layer: albums are *derived* (`albums::regenerate` drops and rebuilds them from
face assignments), so a human could not create a group, name it, or put a file
into it. Export knew only AI albums and a player filter.

**Built** (`d9ec7f2`):

- `media_groups` / `media_group_items` tables and `repo::groups`. A group is a
  name plus pointers into the media index; membership is many-to-many, per
  shoot, counts denormalised the way `albums` does it.
- A **Sort screen**, where a shoot now opens: group panel with per-group folder
  previews, a *Not sorted yet* backlog, a selection-first grid, drag onto a
  group, group chips on each thumbnail, and one click to seed groups from the AI
  player albums (re-runnable, never undoing a manual edit).
- Export defaults to the editor's groups, takes a group selection, and writes a
  `_sorting-report.txt` manifest.

**Decisions worth keeping**

- *Nothing in the AI path may rewrite `media_groups`.* `stages::reset_analysis`,
  `clear_all_recognition_data` and `albums::regenerate` all leave it alone, so
  re-analysing a shoot cannot destroy an afternoon of sorting.
- *A file may belong to several groups.* A clip with two players in it is both
  players' footage. "Move here" exists for the mis-filed case.
- *Folders materialise at export, not at naming.* Re-runs skip what is already
  there, so exporting repeatedly while sorting behaves incrementally.

**Verified:** an end-to-end test that names two groups, exports, and asserts the
folders hold copies while the source directory is byte-identical.

## 4. Building and launching it — two real bugs

Asked to build and launch, the first attempt showed *"Hmmm… can't reach this
page"*. Two distinct causes, both worth recording:

1. **`cargo build --release` is not a production Tauri build.** Tauri decides
   dev-vs-production from the `custom-protocol` cargo feature
   (`let dev = !custom_protocol` in `tauri/build.rs`), which only the Tauri CLI
   sets. A plain cargo release build points the window at `localhost:1420`.
   Always build through `npm run tauri:build`.
2. **A migration-number collision.** `V1` — three commits ahead in the main
   working tree — had already claimed migration version 2 for `person_count`.
   The live database was at `user_version = 2`, so the manual-grouping migration
   was skipped as "already applied" and the group tables were never created.
   Fixed by merging `V1` and renumbering to 3 (`c60e57b`).

The second is the one to remember: **check the highest migration version in use
before claiming a number.** A collision fails silently on exactly the databases
that matter — the ones with data in them.

## 5. Deployment (`3474080`, `746e7dc`)

- `.github/workflows/release.yml` — a `v*` tag builds macOS (Apple Silicon,
  `macos-14`) and Windows and attaches both installers to a draft release. A
  macOS app cannot be built on Windows; CI or a Mac is the only route.
- `scripts/build-macos.sh` — the same build on the Mac itself, with prerequisite
  checks that turn a confusing failure into a sentence.
- **Model bundling.** `models::seed_from_bundle` installs bundled ONNX models
  into the app data folder on first run. Kept in a separate config overlay
  (`tauri.models.conf.json`) because the models are gitignored and Tauri treats
  a resource glob matching nothing as a hard build error.
- **DirectML.** Windows carries only DirectML 1.0 in System32 while ONNX Runtime
  wants the 1.15 redistributable it downloads at build time; without it an
  installed copy silently runs on CPU (~2.6× slower detection, ~7.9× embedding).
  `scripts/stage-directml.mjs` copies it out of the download cache — it cannot
  be referenced inside `target/`, which cargo is writing while the bundler reads.
- **A bug I introduced and then found:** `tauri.windows.conf.json` is a
  *reserved platform name* that Tauri auto-merges on Windows, so the DirectML
  resource was being pulled into every dev build and would have broken
  `npm run dev`. Renamed `tauri.directml.conf.json`.

Signing is not done: both installers are unsigned, so macOS needs
right-click → Open (or `xattr -dr com.apple.quarantine`) and Windows shows
SmartScreen. The workflow already passes all six Apple secrets through, so
adding them as repository secrets is the whole job.

## 6. The server refactor — one core, two front doors

Goal: keep the desktop fast, make a NAS-hosted edition possible without forking.
Phases 0–4 of the task list are done; 5–7 are not started.

```text
   screens · api.ts · media.ts · eventBridge.ts        (one React bundle)
                        │
                transport() — chosen once at boot
                ┌───────┴────────┐
        DesktopTransport      HttpTransport
        HTTP to loopback      HTTP to a NAS
        + native dialog       + jailed fs browser
                        │
                   skwad-server  (58 routes, SSE, media, auth)
                        │
                   skwad-app-core  (state · settings · worker · pipeline ·
                                  stages · export · models · paths · media)
                        │
   database · media-core · face-detection · face-recognition ·
   clustering · video-analysis · export-engine
```

### Phase 0–1: extracting the core (`4357b69`, `07bce27`)

`crates/app-core` holds everything that does not know what a front door is.
Moved verbatim in one commit and rewired in the next, so the relocation is
reviewable on its own. Three seams were cut:

1. **Events.** `ProgressSink` replaces `AppHandle::emit`. The sink lives on
   `AppState`, so the worker pool, monitor and export runner reach it via
   `state.sink()` rather than carrying a handle. Event names and payloads are
   unchanged, which is what let Phase 3 swap transport without touching a
   handler. `NullProgressSink` and `RecordingProgressSink` are the doubles.
2. **Paths.** `AppState::new` takes an explicit `AppPaths`; nothing in the core
   asks a resolver where the data directory is.
3. **Signatures.** `WorkerPool::start(state)` and `export::start(state, …)` lost
   their `AppHandle`, as did every command that only held one to emit with.

The resource policy was deliberately not touched: two workers, worker 0 owning
the single detector/embedder pair and the finishing stages, inference threads
capped at four, lazy model load, 30-second idle unload, rebuild on
`settings_version`, ~500 ms progress throttle.

### Phase 2: the HTTP server (`42b3d0f`, `6e0f4df`, `399184d`)

All 62 commands ported one-to-one, grouped as `commands.rs` groups them.
Four documented deviations, none a redesign: pause is process-wide so it lives
at `/api/processing/pause`; there is no separate `scan` because `resume`
re-enqueues it; `/api/jobs/summary` is new because no command matches it; and
the grouping commands postdate the task list's table.

- **Media.** The id lookup, HEIC/raw render and `Range` arithmetic moved into
  `skwad_app_core::media`, so `skwadmedia://` and `/media/{id}/…` are two adapters
  over one implementation. Ids remain the only thing either accepts.
- **Events.** `/api/events` is SSE over a broadcast channel. A lagging
  subscriber drops the gap, because every payload is a snapshot, not a delta.
- **Filesystem browser.** `/api/fs/{roots,list}` canonicalises every path and
  confirms containment, so `..` and symlinks fail closed; a parent link is only
  offered while it stays inside a root; listings are one level deep.
- **Export guard.** `validate_destination` now compares `(dev, ino)` as well as
  canonical paths, because a container commonly mounts one share twice
  (`/media` read-only, `/output` writable) and a path check cannot see through a
  bind mount. Unix only — Windows has no stable file index outside nightly.
- **Auth.** One bearer token over `/api/*` and `/media/*`, constant-time
  compared, from `SKWAD_TOKEN` or generated to `<data>/token` at `0600`.

### Phase 3: one bundle, two transports (`ec3c7f2`)

`Transport` is five methods: `call`, `listen`, `mediaUrl`, `setMediaBase`, and
an optional `pickFolder`. Command *names* stay the contract, so
`transport/routes.ts` is the only file that knows the API is HTTP; it mirrors the
server's route list for diffing.

- `nullOn404` marks the few commands that return `T | null` on the desktop where
  the server answers 404.
- Media URLs differ in *shape*, not just prefix (`skwadmedia://thumb/7` against
  `/media/7/stream`), so `media.ts` asks the transport for "the thumbnail of
  this id".
- `pickFolder`'s absence is how `PathPicker` knows to open the server's jailed
  browser — a capability check, not a platform check.
- `ConnectionScreen` remembers a connection, retries it silently, and
  distinguishes "token refused" from "nothing answered", because the fix differs.

### Phase 4: the shell becomes a client (`3b0d45c`, `549875c`)

`apps/desktop/src-tauri` is **601 lines**: a window, a log, a supervisor and
three commands (`server_status`, `reveal_in_folder`, `open_path`). The Tauri
command layer is deleted — Phase 2's deliberate duplication is resolved — and the
shell binary fell from **32 MB to 5.7 MB** because it no longer links the
database, the AI crates or ONNX Runtime. That weight is now `skwad-server.exe`
(27.5 MB); the installer ships both.

The supervisor spawns `skwad-server` on `127.0.0.1:0` with a token generated per
launch and never written to disk, learns the bound port through `--port-file`
(a parent that asked for port 0 has no other way, and pre-picking one would
race), forwards the child's output into `logs/skwad.log`, restarts it if it dies,
and kills it when the window closes.

**The webview keeps loading the embedded bundle**, so a first paint never waits
on a port. That makes every request cross-origin — page on `tauri.localhost`,
server on `127.0.0.1` — which surfaced two things a same-origin browser test
could not:

1. The server needed **CORS** for the Tauri webview origins, with credentials
   (which rules out a wildcard).
2. **`SameSite` cookies are never sent cross-site**, so `EventSource` and `<img>`
   cannot authenticate by cookie. They carry `?token=…`, accepted as a third
   option after header and cookie — loopback only, one launch long.

**Upgrade safety (task 4.4):** the data layout is untouched, so an existing
install opens its existing `media.db` and continues from whatever migration it
was on. Confirmed against the real 19.8 MB database on this machine.

## 7. Naming a person gathers their footage (`eba8f5f`, `ea90d8f`)

The first attempt read the request as click-to-tag and shipped that: face boxes
in the viewer became clickable, unnamed ones dashed and amber, with a popover to
name one face. **That was the wrong scope** — the correction was that naming must
*propagate*: one answer should gather every photo that person appears in.

`POST /api/faces/name` is now the whole loop in one call, because four round
trips leave the library half-updated when one fails:

1. the name becomes a player, reused when already known;
2. every face in the same unknown cluster is assigned to them — a cluster is one
   person by construction, so one answer names them everywhere the clusterer
   found them;
3. albums regenerate, which is what knows every file a player appears in;
4. a group named after them is created or topped up from that album.

Two entry points and the click-to-tag popover all call it, so the quick path and
the guided path cannot drift. The guided flow reads the faces first and asks
*which* person is being named, showing a crop of each, then returns to that list
after every answer with a running tally.

---

## 8. Bugs found, and how

Worth reading as a list, because each was found by a different kind of check:

| Bug | Found by | Lesson |
| --- | --- | --- |
| Migration 2 collision with `V1`'s `person_count` | Launching the app and inspecting the real database | Tests on fresh databases cannot see a version collision |
| `cargo build --release` builds a dev Tauri app | A screenshot from the user of the running window | Read the framework's own build script before assuming |
| `tauri.windows.conf.json` auto-merges | A `cargo check` failing on a locked DLL | Reserved filenames are a real category of bug |
| No CORS for the Tauri origin | Launching the packaged desktop app | Same-origin browser tests hide cross-origin failures |
| `SameSite` blocks cookie auth cross-site | The same launch, one layer deeper | "It works in the browser" is not "it works in the webview" |
| `\\?\` prefix leaking into export reports | Reading a generated report | Canonicalising has a cosmetic cost worth undoing |
| Version-skewed sidecar rejecting a flag | A packaged launch showing "exit code 1" | Quote the child's own words in the error |
| Stale binary misread as a routing bug | Curling a route that should have existed | `cargo check` is not `cargo build` |

## 9. What was actually verified

Not "the tests pass" — these were run against real data on real hardware:

- **Manual grouping:** the user's own run — group "Spero", 44 photos, exported to
  `\\TESS-CREATIVE-12bay\For Editors\Sort Test\Sorted`, 974 MB, `Spero/Photos/`
  plus `_sorting-report.txt`, source folder 298 files and **0 subfolders**.
- **Server, by curl:** token enforced (401 with none or wrong); `C:\Windows` and
  a `..` escape both 403; a shoot driven `scanning → analysing → complete` with
  11 faces over 3 photos; 37 SSE events (32 progress, 4 shoot-changed, 1 notice);
  a 27,666-byte JPEG thumbnail; an export of 2/2 files with its report.
- **Browser edition, in a real browser:** connection screen → connected →
  shoot created **through the jailed folder picker** → processing complete →
  sorted → exported; a reload reconnected silently from the saved connection.
- **Desktop as client:** server on a random loopback port with a 64-character
  per-launch token, the real 297-file library intact, thumbnails over loopback.
  Killing the child server restarted it on a new port and the UI recovered with
  no reload and no error screen.
- **Naming, on a copy of the real database** (because it writes): naming one
  face in a five-face photo matched 71 faces and gathered **72 files** into that
  person's group; a second face gathered 20 more; the shoot went 0 → 80 sorted in
  two answers. The copy was then discarded, and the live library confirmed
  unchanged.

**Current gate:** 277 tests across ten crates (`app-core` 57, `database` 50,
`clustering` 30, `server` 26, `export-engine` 24, `face-detection` 20,
`media-core` 18, `face-recognition` 17, `video-analysis` 11, `desktop` 4),
`cargo clippy --workspace --all-targets -- -D warnings` clean, `npm run
typecheck` and `npm run web:build` clean.

## 10. Known gaps

**Not done, by plan:** task-list phases 5 (NAS container), 6 (remote GPU worker)
and 7 (multi-user hardening).

**Not verified by me, and why:**

- **The macOS build.** No Apple toolchain on this machine. CI (`macos-14`) or the
  Mac itself has to produce and test it.
- **The native folder dialog** in the desktop build — clicking it would have left
  a modal open on the user's machine. The code path is type-checked and present.
- **The NSIS installer since the server split.** The last full installer was
  built before the shell/server split, so its measured contents (192 MB) predate
  `skwad-server.exe` being part of it. `npm run package:win` stages everything;
  it needs one clean run, with the app closed.
- **Anything on a Synology.**

**Behavioural caveats worth knowing:**

- **A cluster is treated as one person.** That is what makes one answer worth 72
  files, but a clusterer that merged two people will name both. Review → split,
  and *Wrong person* on any face, are the corrections.
- **Groups are topped up, never pruned.** Naming again after more analysis adds
  files and never removes one taken out by hand — manual edits win — so a
  mis-named cluster leaves files behind until removed.
- **`?token=` in URLs** on the desktop only. Loopback, one launch. Not used by
  the browser edition, where everything is same-origin.

**Repository hygiene, outside this branch:**

- `docs/current-application.md` is **untracked** in the main working tree — in no
  branch, one `git clean` from gone — and now describes neither manual grouping
  nor the naming flow nor the server split.
- The main working tree has ~174 lines of **uncommitted** `V1` work touching
  `AlbumsScreen`, `ExportScreen`, `store.ts`, `styles.css`, `Sidebar` and
  `ShootsScreen` — the same files this branch rewrote. Committing it first will
  make the merge tractable.

## 11. Picking it up

```bash
# verification gate, in the order the task list asks for it
npm run typecheck && npm run web:build
npm run rs:test && npm run rs:check && npm run rs:clippy

# run the desktop app (server sidecar included)
cargo build --release -p skwad-server
node scripts/stage-sidecar.mjs
npm run tauri:build -w @skwad/desktop -- --no-bundle --config src-tauri/tauri.sidecar.conf.json

# run the browser edition against a throwaway data directory
./target/release/skwad-server.exe --bind 127.0.0.1:8420 --data-dir ./.skwad \
  --media-roots D:\shoots --output-roots D:\sorted --token dev --web-dir apps/desktop/dist

# a shippable Windows installer (close the app first — it holds both binaries)
npm run package:win
```

**Where things live**

| Concern | File |
| --- | --- |
| The refactor's design and phase status | `docs/server-architecture.md` |
| Shipping either edition | `docs/deployment.md` |
| Invariants and code-layout notes | `docs/development.md` |
| Manual grouping spec | `docs/architecture-plan.md` §34 |
| Group tables and queries | `crates/database/src/repo/groups.rs` |
| Naming-and-gathering | `crates/server/src/api.rs::name_face` |
| Route table (server) | `crates/server/src/lib.rs::protected` |
| Route table (client) | `apps/desktop/src/transport/routes.ts` |
| Child-process supervision | `apps/desktop/src-tauri/src/supervisor.rs` |

---

## 12. Appendix — how the conversation went

Sections 1–11 are organised by concern. This one is chronological: what was
asked, in the requester's own framing, and what changed as a result. It exists
because the two course corrections below are the most useful thing in this
document and they are invisible in the commit history — a commit shows what was
built, not what was built *instead of* something else.

The whole thing was one session on 23–24 August 2026, interrupted once by a
usage limit and resumed.

### 12.1 The requests, in order

| # | Asked | What happened |
| --- | --- | --- |
| 1 | Describe the app's purpose — cut down the time editors spend sorting footage by whose it is; name groups in the app; those names become folders in a **new** directory; the raw footage folder is a source and must not be altered. *"check where is application now as per this requirement and start working where it lacks"* | Audited the app against the requirement. Found the AI half done and the editor's half missing entirely: albums are derived and cannot hold a human decision. Built manual grouping (§3). |
| 2 | *"Build the changes into app and launch it"* | Two bugs surfaced before it ran: `cargo build --release` produces a *dev* Tauri app, and the manual-grouping migration collided with `V1`'s migration 2 (§4). After fixing both, the app ran on the real library. |
| 3 | *"launch the tauri and the inbuilt is chromium based"* | Took the hint: attached to the WebView2 instance over the Chrome DevTools Protocol, which became the verification method for every later phase — reading the real DOM instead of asserting a build succeeded. |
| 4 | *"Now give me a way to deploy it for others to use. I need a mac studio version"* | CI workflow building macOS (Apple Silicon) and Windows, a Mac build script, signing and notarisation documented, models and the DirectML redistributable bundled, and a Windows installer built and inspected (§5). Stated plainly that no macOS build could be produced or tested from this machine. |
| 5 | Dropped `TASKS-server-refactor.md` — a seven-phase plan for "one core, two front doors" — with *"Start with it"* | Read the plan and the reference docs it names, then worked phases 0–1: extracted `skwad-app-core`, replaced Tauri events with `ProgressSink` (§6). |
| 6 | *"start with Phase 2"* | The HTTP server: 62 commands ported one-to-one, SSE, media routes, a jailed filesystem browser, bearer auth. Driven end to end with `curl` against a real shoot. |
| 7 | *"Start phase 3"* | One React bundle, two transports. Verified by driving the browser edition in an actual browser — creating a shoot through the jailed folder picker, watching it process, sorting, exporting. |
| 8 | *"Phase 4 start"* | The desktop shell became a client of its own loopback server; `src-tauri` fell to 601 lines and 5.7 MB. Cross-origin realities (CORS, `SameSite`) only appeared here, because only here is the page a different origin from the API. |
| 9 | *"a small add on. When naming a photo if there are multiple people in it it shall ask for which particular person am I looking for… like how we tag in instagram"* | Built click-to-tag on the face boxes in the viewer. **Wrong scope** — see 12.2. |
| 10 | *"No the sequence shall be…"* — select a photo, read its faces, ask which person is being named, and on naming, **a group of that person's photos is prepared**; name another face and the same happens for them | Rebuilt it as a propagating operation (§7): one answer names the person everywhere the clusterer found them and gathers every file they appear in into a group. Measured at 72 files from one answer. |
| 11 | *"Make a doc of this chat's whole context"* | This document. |
| 12 | *"I cant find the worklog doc written in docs"* | It was in the worktree, not the main checkout — see 12.3. Copied the three new docs into the main folder and explained how to bring the code across. |

### 12.2 The two corrections, and what they teach

**Naming was scoped as tagging.** Request 9 described clicking a person in a
photo, "like how we tag in Instagram", and that was built literally: clickable
face boxes, a popover, one face assigned per answer. Request 10 made the actual
requirement explicit — naming has to *propagate*. The point of identifying
someone is not the label; it is that their footage ends up in a folder. One
answer now matches every face in that person's cluster and gathers every file
they appear in.

The tell was in the original wording and was missed: *"a group of such persons
photos shall be prepared"*. The lesson is not "ask more questions" — it is that a
request describing a **mechanism** ("click the person") usually also states an
**outcome**, and the outcome is the requirement. Mechanism without outcome ships
something that demos well and saves nobody any time.

**The task list's route table conflicted with the code.** Phase 2 asked for
`POST /api/shoots/:id/pause`, but pausing is process-wide in the core, so a
per-shoot URL would have been a lie. Four such deviations were built differently
and written down with reasons rather than followed to the letter or silently
dropped. The plan was a good plan; it was written before the code it describes
had settled.

### 12.3 Why the doc could not be found

Every change in this session was made in a git worktree —
`.claude/worktrees/footage-grouping-nas-org-25d54b`, branch
`feat/server-extraction` — while the main checkout stayed on `V1`. The desktop
app that was launched and used throughout was built *from* that worktree, so the
features were real and working while none of the source or docs appeared in the
main folder.

Worth knowing for next time: the main checkout also holds roughly 174 lines of
uncommitted `V1` work touching six of the same frontend files this branch
rewrote. Merging on top of that without committing it first would tangle them,
which is why the merge was left as a decision rather than performed.

### 12.4 What verification looked like in practice

The pattern that caught the most, in rough order of how much each found:

1. **Run the real thing against real data.** The migration collision, the dev
   build, the CORS failure and the cookie failure were all invisible to the test
   suite and obvious on launch.
2. **Drive the actual UI**, not the API underneath it. The browser edition was
   verified by clicking through a shoot creation in a real browser; the desktop
   by scripting its webview over CDP.
3. **Write to a copy when the operation writes.** The naming flow was measured on
   a duplicate of the live database precisely so the real one could be left
   untouched — then confirmed untouched afterwards.
4. **Then the gate:** 277 tests, clippy with `-D warnings`, typecheck, bundle
   build. Necessary, and last.
