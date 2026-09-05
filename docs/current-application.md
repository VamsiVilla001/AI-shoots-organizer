# SKWAD Media Organiser — Current Application Documentation

**Application version:** 1.2.0 (development)

**Documentation snapshot:** 23 August 2026  
**Status:** Version 1.2 editor workflow under active development; 1.0.0 is the stable baseline

This document describes the application as it is implemented in the current
repository. It is the canonical guide to current behavior. The separate
`architecture-plan.md` file is a historical product and architecture plan and
may contain ideas that are not implemented yet.

## 1. Product summary

SKWAD Media Organiser is a desktop application for sorting large esports
photo and video shoots by the people visible in them. It scans an existing
shoot folder, detects and recognises faces locally, groups unknown people for
review, generates virtual albums, and copies selected groups into organised
destination folders.

The intended workflow is:

```text
Create shoot
  → scan and index media
  → generate thumbnails
  → detect and embed faces
  → match known players
  → cluster unknown faces
  → name/review people
  → browse person or group-size albums
  → copy selected groups into organised folders
```

The application is deliberately local-first:

- AI inference runs on the user's computer.
- No media, face crops, embeddings, names, or metadata are uploaded.
- Source photos and videos are indexed in place.
- Source files are never moved, renamed, modified, or deleted by normal app
  operations.
- “Copy & Organise” creates copies in a user-selected destination.

## 2. Current technology stack

| Layer | Implementation |
| --- | --- |
| Desktop shell | Tauri 2 |
| User interface | React 18, TypeScript, Vite |
| UI server state | TanStack Query |
| Transient UI state | Zustand |
| Backend | Rust 2021 |
| Database | SQLite with WAL and migrations |
| AI runtime | ONNX Runtime |
| Face detection | InsightFace SCRFD `det_10g.onnx` |
| Face recognition | InsightFace ArcFace `w600k_r50.onnx` |
| Media decode | Rust `image` crate, LibRaw for camera RAW, plus FFmpeg |
| Video sampling | FFmpeg scene detection and interval sampling |
| Windows acceleration | DirectML, with CPU fallback |
| macOS acceleration | CoreML, with CPU fallback |

## 3. Platform support

### Windows

Windows 10/11 is the primary currently validated platform. The application
uses DirectML automatically when available and falls back to CPU inference if
the provider cannot initialise. The configured Windows bundle is an NSIS
installer using per-machine installation.

### macOS, Mac mini, and Mac Studio

The repository contains a macOS application/DMG target, requires macOS 11 or
newer, supports the standard macOS application-data path, and enables the
CoreML execution provider. The intended Mac target is Apple Silicon, including
Mac mini and Mac Studio.

The code path is present, but production distribution still requires normal
macOS validation on real devices, code signing, notarisation, and a final
installer acceptance test. A local source build should be treated separately
from a signed public release.

### Hardware guidance

The accurate bundled models are relatively large. GPU acceleration is strongly
recommended for substantial shoots, but CPU fallback remains functional. The
current scheduler uses at most two background workers and only one AI model
pair, preventing multiple ONNX sessions from competing for RAM and GPU memory.

## 4. Main application areas

The sidebar exposes the following workspaces.

### 4.1 Shoots

A shoot is a named index pointing at one source directory. The Shoots screen
shows:

- shoot name and creation date;
- processing status and percentage;
- photo and video counts;
- known player and unknown-cluster counts;
- failed-job count;
- Open, Resume, Copy & Organise, and Delete Index actions.

Creating a shoot requires a source folder and a name. The folder name is used
as a suggested shoot name after choosing it with the native folder picker.
Scanning begins immediately in the background.

“Delete Index” deletes that shoot's database records and derived analysis. It
does not touch the source directory.

“Clear scanned data” removes every shoot index, per-shoot faces, clusters,
albums, jobs, export records, and generated thumbnails. It retains application
settings, log records, installed model files, and player profile rows. Because
face samples belong to shoots, clearing the shoot indexes also removes those
per-shoot samples. Original media remains untouched.

### 4.2 Players

The Players workspace is the reusable local identity library. A player profile
contains:

- name;
- optional team;
- optional notes in the data model;
- confirmed face-sample count;
- media count;
- number of shoots in which the player appears.

Players are normally created by naming an unknown cluster or assigning faces
from Review. The Manage dialog supports:

- renaming a player;
- updating the team;
- merging a duplicate profile into another player;
- deleting recognition data while keeping the profile;
- deleting the player profile entirely.

Confirmed assignments become library samples used to recognise the player in
future shoots.

### 4.3 AI Albums

Albums are virtual database groupings; they do not duplicate source files.
They can be regenerated from the current face assignments at any time.

The screen has an explicit grouping selector and **Apply grouping** button:

- **Face / person** shows named player albums, multi-player combinations,
  optional team albums, and unknown groups needing review.
- **Number of persons** shows files in mutually exclusive buckets: No persons,
  Single, Two persons, and so on through 10+ persons.

A file containing several named people appears in every relevant player album.
This is intentional: a group photograph containing Person A and Person B is
useful in both people's collections.

Opening an album allows filtering by All, Photos, or Videos. Person albums can
also be cross-filtered by visible group size, for example a player's solo
photographs only.

For recognised player albums, image tiles show match confidence:

- `Match 87%` represents the stored ArcFace cosine similarity shown as a
  percentage-like score;
- `Reference` means the face was assigned manually and no synthetic AI
  confidence is invented.

The confidence is a similarity score used by the matcher, not a calibrated
statistical probability that the identity is correct.

### 4.4 Unknown groups and naming

Unmatched faces are clustered into “Unknown Person” groups. Naming a cluster:

1. creates or reuses a player profile by name;
2. confirms all faces in the cluster for that person;
3. adds those face embeddings to the reusable library;
4. regenerates albums;
5. makes future shoots able to suggest that identity.

Small groups below the configured minimum size remain in the unidentified
pool rather than being forced into a cluster.

### 4.5 Review

Review displays face crops rather than whole media files. The available views
are Suggestions, Unknown, Confirmed, and Everything. Suggestions are ordered
for practical review and show their match confidence range.

Review operations include:

- select one or more faces;
- accept suggested matches;
- reject a wrong-person match and return it to the unknown pool;
- assign selected faces to an existing or new player;
- mark false detections as “Not a face”;
- confirm all currently visible suggestions;
- double-click a face to open its source media in the viewer.

Human-confirmed assignments are protected from normal recognition and
clustering regeneration. Re-running derived stages must not silently undo a
human decision.

### 4.6 Copy & Organise

Copy & Organise is the output workflow. It copies original media into a new
directory and never moves or changes the source files.

Named player albums are selectable directly on the Albums screen. The user can:

- select individual person groups;
- select or clear all named players;
- click **Copy selected groups…**;
- copy one open person album with **Copy this person's group…**;
- open Copy & Organise independently from the sidebar for an unfiltered run.

A selection made in Albums carries the exact person IDs into Copy & Organise.
The destination screen previews file count, total bytes, and folder count
before enabling the copy.

Default output:

```text
Chosen destination/
├── Person One/
│   ├── Photos/
│   └── Videos/
├── Person Two/
│   ├── Photos/
│   └── Videos/
└── Unidentified/          optional
```

Available options are:

- include every player or select specific players;
- create Photos and Videos subfolders or put media directly in the person
  folder;
- include or exclude Unidentified;
- optionally include multi-player albums, which creates additional copies;
- optionally include group-size folders, which can duplicate the whole shoot
  along the second grouping axis;
- preserve file modification times where the destination supports them;
- skip, rename, or overwrite a destination file that already exists.

Folder names are sanitised for portability, including Windows-invalid
characters and device names. Duplicate filenames within one output folder are
deduplicated safely. The same original can be copied to several person folders
when several people appear in it.

The app refuses a destination equal to, or nested inside, the shoot source
folder. This prevents output copies from polluting the source and being picked
up by a later rescan. Copying runs on a background thread with live progress,
cancellation, completion history, and an Open folder action.

### 4.7 Settings

Most AI settings apply without restarting the application. Saving bumps a
settings version; workers unload and rebuild their AI sessions as required.
The background worker count determines the pool created at startup, so a
worker-count change takes full effect on the next application launch.

The screen contains:

- acceleration provider;
- worker count and analysis image dimension;
- recognition threshold and runner-up ambiguity margin;
- optional auto-confirm threshold;
- one-person-per-frame matching constraint;
- clustering similarity and minimum cluster size;
- video enablement, sample interval, scene threshold, and frame cap;
- detected models and FFmpeg status;
- application data path and cache size;
- thumbnail-cache and recognition-data controls.

## 5. Media support

Extension matching is case-insensitive.

### Native still-image decoding

`jpg`, `jpeg`, `png`, `webp`, `tif`, `tiff`, and `bmp` are decoded by the Rust
image library and do not require FFmpeg.

### LibRaw camera decoding

Camera RAW formats including `raf`, `arw`, `nef`, `cr2`, `cr3`, `orf`, `rw2`,
`dng`, `pef`, `srw`, `3fr`, `iiq`, `rwl`, and common additional RAW containers
use a preview-first LibRaw pipeline. See [Camera RAW support](raw-support.md).

### FFmpeg still-image decoding

`heic`, `heif`, and `avif` require an FFmpeg build capable of decoding the
specific format.

### Video

`mp4`, `mov`, `mkv`, `avi`, `webm`, `m4v`, `mpg`, `mpeg`, `wmv`, `mts`, and
`m2ts` require FFmpeg.

Recognition can process native and camera RAW photos when FFmpeg is absent.
Video and FFmpeg-dependent still formats remain unavailable until configured.

## 6. Scan and processing pipeline

### 6.1 Scan

The scanner walks the selected directory recursively by default. It records
path, filename, type, extension, size, modification time, and a content key
derived from path/size/mtime. Unchanged files can therefore be recognised on a
rescan without hashing every byte of large media files.

Database inserts and job enqueues are committed in batches of 200. This avoids
holding SQLite's writer lock for an entire large shoot and keeps UI reads and
progress updates responsive.

### 6.2 Metadata and thumbnails

The indexing job reads dimensions, duration, orientation, camera metadata when
available, and creates a cached thumbnail. EXIF orientation is applied before
analysis so thumbnails, full previews, face boxes, and model input share one
coordinate system.

### 6.3 Face detection

Photos are decoded into an analysis copy capped by the configured maximum
dimension, 1600 pixels by default. SCRFD then letterboxes the image to its
640×640 default input without changing aspect ratio.

The detector produces:

- a bounding box;
- detection confidence;
- five facial landmarks when present.

Non-maximum suppression removes overlapping duplicate detections and the
per-image face cap prevents pathological output.

### 6.4 Alignment and embeddings

Each detected face is aligned to the standard ArcFace 112×112 landmark
template using a similarity transform. If landmarks are unavailable, a padded
bounding-box crop is used as a lower-quality fallback.

The recognition model returns a normalised embedding vector, normally 512
dimensions for `w600k_r50`. Embeddings are stored in SQLite as little-endian
`f32` blobs.

GPU providers use batch size 1 because the supplied model and DirectML require
static shapes. CPU inference may process up to eight aligned faces per batch.

### 6.5 Known-player matching

The matcher compares a detected embedding against confirmed library samples.
A suggestion must:

- meet the recognition similarity threshold;
- beat the second-best candidate by the ambiguity margin;
- respect the optional constraint that one player cannot occupy two faces in
  the same frame.

The default `autoConfirmAbove` value is 1.0, effectively disabling automatic
confirmation. Matches are therefore suggestions awaiting review unless the
user explicitly lowers that setting.

### 6.6 Unknown clustering

Unknown embeddings form a similarity graph from their nearest neighbours.
Deterministic Chinese-Whispers-style label propagation groups connected faces,
then highly similar cluster centroids can be merged. Human-named clusters are
preserved when unknown clustering is regenerated.

### 6.7 Video analysis

Videos are not analysed frame by frame. FFmpeg detects scene changes and the
planner combines those cuts with periodic samples. Nearby timestamps are
deduplicated, the first frame is included, and the configured maximum frame
count is enforced. Each selected frame then passes through the same detection,
alignment, embedding, and matching pipeline as a photo.

Video face rows carry a frame timestamp. Timeline records merge nearby hits
for browsing player appearances without treating every sampled detection as a
different person.

### 6.8 Album generation and people count

Albums are regenerated only after per-file analysis is complete. A media
record stores both `faceCount` and `personCount`:

- `faceCount` is the number of detection rows. In video, the same person can
  contribute one row per sampled frame.
- `personCount` estimates distinct visible people and is used for group-size
  albums.

The people-count calculation combines distinct identities with the maximum
unidentified faces visible in one frame and never lets clustering errors reduce
the result below the maximum faces actually visible together.

## 7. Background jobs, recovery, and responsiveness

The processing pipeline is a persistent SQLite job queue. Job kinds are Scan,
Thumbnail, Analyse Photo, Analyse Video, Recognise, Cluster, and Albums.
Priorities move a shoot from indexing through per-file analysis into
shoot-wide finishing stages.

Claims use an atomic `UPDATE … RETURNING`, preventing two workers from running
the same job. On startup, jobs left in `running` state by a crash are returned
to the queue.

Current resource policy:

- maximum two background workers;
- worker 0 owns AI inference and finishing stages;
- worker 1 handles scanning and thumbnails when enabled;
- scanning/thumbnails can overlap GPU inference;
- only one detector/embedder pair is loaded;
- inference threads are capped at four per session;
- at least one logical CPU is reserved for the UI and supporting work;
- models load lazily on the first AI job;
- model sessions unload after 30 seconds with no AI work;
- progress events are emitted roughly every 500 ms.

If required models or FFmpeg are missing, work is treated as blocked rather
than consuming every job's retry budget. Notices are throttled and the worker
checks again after a backoff. Individual failures retry up to the queue's
attempt limit and remain visible as failed jobs when exhausted.

Pause, resume, cancel, and reanalyse are supported by the backend. Reanalysis
clears derived AI results for one shoot, keeps its media index, and queues the
pipeline again.

## 8. Default settings and valid ranges

| Setting | Default | Sanitised range/behavior |
| --- | ---: | --- |
| Accelerator | Automatic | Auto, CPU, platform GPU, optional CUDA |
| Background workers | up to 2 | 1–2 and no more than available CPUs |
| Inference threads | hardware-derived | 1–4 |
| Detection threshold | 0.50 | 0.05–0.99 |
| Detection NMS threshold | 0.40 | 0.10–0.90 |
| Detection input | 640 | 320–1280, rounded to multiple of 32 |
| Max faces per image | 64 | 1–256 |
| Analysis longest edge | 1600 | 640–4096 |
| Recognition threshold | 0.42 | 0.10–0.99 |
| Recognition margin | 0.05 | 0.00–0.50 |
| Unique player per frame | enabled | Boolean |
| Auto-confirm above | 1.00 | 0.00–1.00 |
| Cluster edge similarity | 0.45 | 0.10–0.99 |
| Minimum cluster size | 3 | 1–100 |
| Cluster merge similarity | 0.62 | 0.10–1.00 |
| Cluster neighbours | 12 | 2–64 |
| Video analysis | enabled | Boolean |
| Video scene threshold | 0.30 | 0.05–0.95 |
| Video sample interval | 5 seconds | 0–600 seconds |
| Max sampled video frames | 60 | 1–1000 |
| Recursive scanning | enabled | Boolean |

Lower thresholds usually improve recall but increase false matches or false
groups. Higher analysis dimensions help small faces but increase image-decode,
resize, memory, and alignment costs.

## 9. Models and acceleration

The app does not bundle copyrighted model weights in source control. The model
fetch scripts download InsightFace `buffalo_l` and retain:

- `det_10g.onnx` — SCRFD-10G detector with landmarks;
- `w600k_r50.onnx` — ArcFace-compatible ResNet-50 embedder.

These are accuracy-oriented models. The registry can discover other ONNX files
by filename and allows detector/embedder selection through settings data. If no
explicit file is selected, the largest candidate for each role is preferred.

Execution-provider order is GPU first and CPU last. Provider registration is
best effort; failure to create the preferred GPU provider rebuilds a CPU
session instead of crashing the application.

CUDA is optional and requires a compatible local toolkit and a build with the
`cuda` feature. DirectML on Windows and CoreML on macOS are target-enabled by
default.

## 10. Application data and privacy

Managed data is stored under the Tauri application-data directory:

```text
com.skwad.mediaorganiser/
├── database/media.db
├── thumbnails/
├── face_cache/
├── models/
└── logs/skwad.log
```

On Windows this is normally under `%APPDATA%`; on macOS it is under
`~/Library/Application Support`.

SQLite stores shoot indexes, media metadata, face boxes, landmarks, embeddings,
assignments, clusters, people, albums, jobs, copy history, settings, and an
application event log. The event log stores identifiers and outcomes, not
embedding blobs or image crops.

The webview does not receive arbitrary filesystem access. Media is served by a
custom `skwadmedia` protocol that resolves database media IDs and supports byte
ranges for video playback.

## 11. Data-management controls

| Control | Removes | Retains |
| --- | --- | --- |
| Delete Index | One shoot index and its derived data | Original files, other shoots, global settings/models |
| Clear scanned data | All shoot indexes and thumbnails | Originals, settings, logs, models, player profile rows |
| Clear thumbnail cache | Generated thumbnails and stored thumbnail paths | Media indexes, faces, albums, originals |
| Delete all embeddings | Embedding vectors | Detections, assignments, albums, profiles, originals |
| Delete player recognition data | That player's face assignments/samples | Player profile and originals |
| Clear all recognition data | Faces, video detections, clusters, albums, people | Media indexes, source files, settings/models |

Cleared thumbnails can be regenerated by resuming/reprocessing applicable
media. Clearing embeddings removes the vectors required for future matching;
reanalysis is required to recreate them.

## 12. Database model

The primary tables are:

- `shoots` — source folder and workflow status;
- `media` — indexed file metadata, thumbnail/status, face and person counts;
- `people` — reusable named player profiles;
- `faces` — bounding boxes, landmarks, embeddings, identity assignments, and
  confidence;
- `clusters` and `cluster_faces` — unknown-person groupings;
- `video_detections` — timestamped player appearances;
- `albums` and `album_media` — generated virtual collections;
- `jobs` — persistent processing queue;
- `exports` — Copy & Organise history and progress;
- `settings` — JSON application settings;
- `app_log` — operational audit/event log.

Foreign keys are enabled and per-shoot derived records cascade when a shoot
index is removed. Database schema changes are applied through append-only
migrations.

## 13. Architecture and code layout

```text
React UI
  │ Tauri invoke / events
  ▼
Rust command layer
  ├── SQLite repositories
  ├── persistent workers
  ├── media protocol
  └── Copy & Organise runner
       │
       ├── media-core
       ├── face-detection
       ├── face-recognition
       ├── clustering
       ├── video-analysis
       └── export-engine
```

Repository map:

```text
apps/desktop/src/             React screens, components, API wrapper, UI state
apps/desktop/src-tauri/src/   Tauri commands, state, workers, pipeline, protocol
crates/database/              Schema, migrations, repositories, job queue
crates/media-core/            Scanner, metadata, decoding, FFmpeg, thumbnails
crates/face-detection/        ONNX runtime and SCRFD decode/NMS
crates/face-recognition/      Face alignment and ArcFace embeddings
crates/clustering/            Known-player matcher and unknown clustering
crates/video-analysis/        Video sampling and appearance timelines
crates/export-engine/         Safe copy planning, naming, and execution
packages/shared-types/        TypeScript mirrors of Rust IPC structures
models/                       Local ONNX files, excluded from source control
scripts/                      Model fetch and asset helpers
docs/                         Current guide, development notes, historical plan
```

Rust command handlers remain intentionally thin. Long operations are queued or
spawned rather than performed synchronously on the Tauri IPC thread. Rust
records serialise in camelCase and are mirrored by hand in
`packages/shared-types`; both sides must change together.

## 14. Setup and development

### Prerequisites

- Node.js 20 or newer;
- stable Rust with the platform toolchain;
- Tauri platform prerequisites;
- FFmpeg on `PATH` or configured in Settings for video/HEIC;
- approximately 280 MB for the fetched model archive plus application caches.

### Install and fetch models

```powershell
npm install
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1
```

On macOS:

```bash
npm install
bash scripts/fetch-models.sh
```

The scripts copy models into the repository and, after the app data directory
exists, into the installed application's models directory.

### Run

```bash
npm run dev
```

The root Cargo profile optimises all AI-heavy workspace crates even during
development because unoptimised inference and clustering are not
representative of application performance.

### Build packages

```bash
npm run build
```

Tauri targets NSIS on Windows and app/DMG on macOS. A release intended for
third parties still needs platform signing and release validation.

### Validation commands

```bash
npm run typecheck
npm run web:build
npm run rs:test
npm run rs:check
npm run rs:clippy
```

Real-model ONNX tests are ignored by default because they require downloaded
model weights. The normal workspace suite is hermetic and uses in-memory
SQLite databases and temporary directories.

## 15. Operational guidance

### A shoot appears stuck

1. Check the progress panel and failed-job count.
2. Confirm both model files are reported as ready in Settings.
3. Confirm FFmpeg is found if the shoot contains video or FFmpeg-only stills.
4. Use Resume after resolving a missing dependency.
5. Use Reanalyse when changing models or thresholds and a complete rerun is
   intended.
6. Check `logs/skwad.log` for provider fallback or file-specific decode errors.

### The application becomes unresponsive

Keep workers at the default maximum of two and acceleration on Automatic. The
current scheduler intentionally avoids multiple simultaneous model pairs.
Very high analysis dimensions, large RAW files, or CPU-only inference can still
increase latency. Closing other GPU-heavy applications may help DirectML obtain
resources.

### Recognition is inaccurate

- Confirm several clear, varied face samples for each player.
- Correct wrong suggestions instead of accepting them.
- Avoid lowering the recognition threshold aggressively.
- Use the ambiguity margin to suppress close runner-up matches.
- Treat confidence as similarity evidence, not certainty.
- Regenerate albums after identity corrections if the UI has not already done
  so automatically.

### Copy output contains duplicates

This can be correct. A photo containing two players belongs to both player
folders. Enabling multi-player or group-size output creates additional copies
by design. Disable those options when only one folder per named player is
required.

## 16. Current limitations

- Recognition quality depends on source resolution, lighting, pose,
  obstruction, motion blur, and the quality/diversity of confirmed samples.
- ArcFace similarity is not a calibrated probability.
- A source file can appear in multiple virtual albums and output folders.
- Video analysis samples frames and can miss a person visible only between
  samples.
- FFmpeg codec support varies by the installed build; LibRaw camera coverage
  varies by its linked version.
- Models are downloaded separately and increase installation/setup complexity.
- macOS code paths exist, but signed/notarised distribution and real-device
  acceptance remain release tasks.
- Copy & Organise performs filesystem copies; it does not create hard links,
  symbolic links, or move operations.
- The UI currently exposes the most important settings, while several advanced
  sanitised settings exist primarily in the backend data contract.
- The app is a single-user, local desktop workflow; it has no cloud sync,
  shared catalog, or multi-machine player library.

## 17. Safety invariants

The following rules are foundational and should remain true in future changes:

1. Never modify or delete source shoot media.
2. Treat albums, suggestions, counts, and clusters as rebuildable derived data.
3. Preserve explicit human identity decisions during normal regeneration.
4. Keep slow work off the UI/IPC thread.
5. Make processing resumable and idempotent.
6. Keep recognition data local unless a future user explicitly opts into a
   separately designed sync feature.
7. Preview Copy & Organise plans before writing and reject destinations inside
   source directories.
