# TE Organiser application memory

Last updated: 4 September 2026 (Asia/Calcutta)

This is the current handoff memory for the **Esports AI Media Organiser** (also
called **TE Organiser**). It consolidates the product intent, repository state,
implemented work, decisions, test status, known limitations, and next work from
the development sessions. Claude should read this file before changing the
application.

## 1. Current repository state

- Repository: `https://github.com/VamsiVilla001/AI-shoots-organizer.git`
- Local checkout: `D:\Project KK\Personal projects\TE Organiser`
- Active branch: `windowsV2`
- Current committed HEAD: `3e545da feat(media): give RAW photos full feature parity`
- `windowsV2` currently matches `origin/windowsV2` at `3e545da`.
- Stable product baseline: `1.0.0`.
- Current workspace/package/Tauri version: `1.2.0` development.
- Rust edition: 2021. Minimum Rust version: 1.82.
- The working tree is intentionally dirty. It contains substantial V1.2 work
  that has been tested but has **not** yet been committed or pushed.
- Never reset, discard, or overwrite the dirty working tree to “clean it up.”
  Existing changes are the product work described below.

The latest committed history, newest first, is:

```text
3e545da feat(media): give RAW photos full feature parity
6dd7fb3 feat(shoots): clear selected scanned indexes
861e8d4 feat(media): add preview-first LibRaw decoding
c2b6bec fix(pipeline): wait for orientation indexing before analysis
4471ac8 feat(review): add manual face marking and responsive controls
621d74d feat(review): tag a face by clicking it in the photo
a4a797f Merge V1 into manual grouping, and renumber the migration to 3
343bcfc Fix upgrade path for existing version 3 databases
0a914cb Add photo quality and duplicate ranking foundation
8c32639 Establish TE Organiser 1.0 baseline and 1.2 roadmap
24d2906 Optimize scan and face processing resources
1b452ac Stabilize scanning and improve album grouping
```

Important uncommitted additions include video sample-frame review/tagging,
4K-video resource controls, optional OpenCV tracking, frame-aware recognition,
and stricter recognition/group propagation.

## 2. Product purpose and non-negotiable behavior

The application reduces the time esports editors spend sorting large photo and
video shoots by the people visible in them. An editor points the app at an
existing source shoot, names a person once, reviews suggestions, and gathers
that person's media into an editor-named group that exports as a folder.

Non-negotiable invariants:

1. Everything is local-first. Media, face embeddings, names, and metadata are
   not uploaded to a cloud service.
2. Source media is read-only. Normal processing and export must never rename,
   move, modify, or delete source files.
3. Export copies originals into a separate destination with collision-safe
   names.
4. Human-confirmed face decisions outrank AI suggestions and must survive
   normal regeneration.
5. Albums, unknown clusters, suggestions, counts, and jobs are derived state
   and should be safely rebuildable.
6. Manual groups are editor-owned persistent state and are deliberately not
   erased by recognition-data resets or album regeneration.
7. Face similarity is evidence, not a calibrated probability. The UI and logs
   must not describe cosine similarity as guaranteed identity accuracy.
8. RAW, JPEG, PNG, and other still formats must enter the same normalized
   photo-analysis path and receive the same ranking, tagging, grouping, and
   export features.
9. Video tagging is performed on analyzed sample frames; assigning a face in a
   sample groups the complete source video for that person.

## 3. Current architecture (the source of truth)

This checkout is a direct Tauri desktop architecture:

```text
React + TypeScript UI
        |
        | Tauri invoke/events
        v
Rust Tauri commands and AppState
        |
        +---- SQLite database and persistent job queue
        +---- media-core (scan, metadata, decode, thumbnails, FFmpeg, LibRaw)
        +---- face-detection (SCRFD via ONNX Runtime)
        +---- face-recognition (ArcFace via ONNX Runtime)
        +---- clustering (cosine matching and deterministic clustering)
        +---- video-analysis (sampling and optional OpenCV tracking)
        +---- export-engine (copy plans and collision-safe folder output)
```

There is no `teo-server` crate, loopback HTTP API, browser transport, or current
`.github/workflows/release.yml` in this checkout. Some older documents describe
that alternate/server-refactor branch; do not rebuild the current app around
those documents unless the user explicitly asks to restart that architecture.

Repository layout:

```text
apps/desktop/src/                  React/TanStack Query/Zustand frontend
apps/desktop/src-tauri/src/        Tauri commands, state, workers, pipeline
crates/database/                   SQLite schema, migrations, repositories
crates/media-core/                 media routing, metadata, decode, thumbnails
crates/face-detection/             ONNX runtime, SCRFD and NMS
crates/face-recognition/           ArcFace alignment and embeddings
crates/clustering/                 matching, graph construction and clustering
crates/video-analysis/             frame planning/sampling and OpenCV bridge
crates/export-engine/              export plans and safe naming
packages/shared-types/             TypeScript mirrors of Rust IPC models
models/                            local ONNX models; not committed
scripts/                           model, icon and OpenCV setup scripts
docs/                              current and historical documentation
```

When a Rust IPC/database model changes, update its manually maintained
camelCase TypeScript mirror in `packages/shared-types/src/index.ts` in the same
change.

## 4. Main workflow and database model

The persistent job flow is:

```text
Scan -> Thumbnail/metadata -> Analyse photo/video -> Recognise -> Cluster -> Albums
```

The queue is stored in SQLite and recovers jobs left running after a crash.
Worker zero owns the AI engine and shoot-wide finishing stages. A second worker,
when enabled, handles scanning and thumbnails. This prevents multiple large
SCRFD/ArcFace model pairs from competing for RAM, CPU, and GPU memory.

Key tables include:

- `shoots`: indexed source folders and processing summaries.
- `media`: source path/type/metadata, thumbnail/status, face/person counts,
  quality score, perceptual hash, duplicate group, and best-shot marker.
- `faces`: normalized boxes, landmarks, embedding, quality, person/cluster,
  assignment state, and optional video `frame_time`.
- `people`: reusable named identities.
- `clusters`: groups of unknown face embeddings.
- `albums` / `album_media`: regenerated AI views such as player and group-size
  albums.
- `media_groups` / `media_group_items`: persistent editor-owned sorting groups.
- `video_detections`: timestamped video appearances linked to face rows.
- `video_sample_frames`: analyzed/reviewable timestamps for each video
  (migration 6, currently uncommitted).
- `jobs`: persistent processing queue.
- `exports`: copy history.
- `settings` and `logs`: application configuration and local event records.

Assignments are important:

- `unassigned`: not yet matched.
- `suggested`: AI result awaiting review.
- `confirmed`: human-trusted identity/reference.
- `rejected`: rejected suggestion state where applicable.
- `ignored`: not a face; excluded from counts/albums.

`media.face_count` counts face rows. For a video, the same person may create one
row at every sampled timestamp. `media.person_count` estimates distinct people
and is used for `Single`, `Two persons`, ... `10+ persons` albums.

## 5. Media routing and RAW parity

The centralized, case-insensitive routing registry is in
`crates/media-core/src/formats.rs`:

```text
JPEG/PNG/WebP/TIFF/BMP -> Rust image decoder
Camera RAW             -> LibRaw (`rawlib`)
HEIC/HEIF/AVIF         -> FFmpeg still decoder
Video                  -> FFmpeg frame decoder
```

Camera RAW is no longer sent to FFmpeg. Supported registry entries include RAF,
ARW, NEF, NRW, CR2, CR3, ORF, RW2, DNG, PEF, SRW, 3FR, IIQ, RWL, RAW, ERF, MRW,
MOS, X3F, KDC, DCR, and MEF. Actual camera support depends on LibRaw.

RAW decoding behavior:

1. Open each file in its own LibRaw context, including Windows Unicode/UNC
   paths.
2. Prefer an embedded JPEG preview when its long edge is at least the required
   working resolution (normally 640 px or more).
3. Otherwise perform a bounded half-size, fast-bilinear LibRaw demosaic with
   camera white balance and 8-bit sRGB output.
4. Resize in memory, apply orientation, and release large intermediate buffers.
5. Do not create TIFF/JPEG sidecars and do not alter the source RAW.

The dependency is `rawlib 0.7.1`, wrapping LibRaw 0.22.2. Windows uses bundled
static linkage. RAW thumbnails use the existing cache/content key derived from
path, size, and modification time, so unchanged files are not repeatedly
decoded.

Stable RAW errors include `RAW_UNSUPPORTED`, `RAW_CORRUPT`,
`RAW_PREVIEW_UNAVAILABLE`, `RAW_DECODE_FAILED`, and `RAW_OUT_OF_MEMORY`.
Detailed decode/timing information goes to logs instead of dumping FFmpeg stderr
into the UI.

Committed RAW work:

- `861e8d4`: preview-first LibRaw decoding.
- `3e545da`: RAW photos receive normal-photo feature parity, including quality
  and duplicate ranking.

Real-file RAW tests are opt-in because proprietary camera originals are not in
the repository:

```powershell
$env:TEO_RAW_FILE='\\server\share\shoot\DSCF1092.RAF'
cargo test -p teo-media-core --test real_raw_files -- --ignored --nocapture
```

The complete real-camera acceptance matrix across RAF, ARW, NEF, CR2/CR3 and
DNG is still a release task.

## 6. Face detection, alignment, manual marking, and orientation repairs

The primary detector is SCRFD (`det_10g.onnx`). It emits boxes, confidence and
five landmarks. ArcFace (`w600k_r50.onnx`) aligns to a 112x112 template and
produces normalized embeddings, normally 512 dimensions.

Faces are stored as normalized 0..1 boxes so the same coordinates work on an
analysis image, thumbnail, full preview, or video sample frame.

Completed editor work:

- Detected boxes are drawn directly over the full-media viewer.
- Clicking a box opens the naming/correction controls.
- The editor can name a person, choose an existing person, mark “not a face,”
  or correct a wrong identity.
- If detection misses or points to the wrong place, the editor can enter manual
  marking mode and drag a box around the actual face.
- Manual boxes are clamped, validated, embedded, persisted with `source =
  'manual'`, and preserved when detector analysis is rerun.
- The face-name/group panel was made responsive and vertically scrollable so it
  is not cropped in short/narrow windows.
- The Sort into Groups screen retains best-shot/quality information while also
  keeping the face naming workflow.
- Video sample frames now use the same click/draw/tag experience as photos.

Orientation repair order requested by the user:

1. Prevent analysis until metadata/orientation indexing completes.
2. Re-read orientation defensively during analysis.
3. Run focused landmark detection inside manually drawn boxes.
4. Quality-filter/cap reference samples and require review for propagation.
5. Back up the database and reanalyze affected rotated photographs.

Current status of that order:

- Steps 1 and 2 are implemented. Analysis jobs defer until indexing has
  completed, then photo/video analysis re-reads source orientation and corrects
  stale metadata before decoding.
- Manual boxes are embedded, but focused landmark detection inside the manual
  box is **not complete**. The current manual embedding constructs a detection
  with `landmarks: None`, so ArcFace uses the bounding-box fallback crop.
- Step 4 is partly implemented and tightened further in the current dirty tree:
  face-tag clicks confirm only the selected face, reference vectors are quality
  filtered/capped, and only the cover of an explicitly named cluster becomes a
  reusable reference.
- Database backup and a targeted reanalysis tool for already affected rotated
  photos remain incomplete.

## 7. Recognition and grouping: current strict behavior

This area was tightened after false faces appeared in named groups.

Current defaults in source are:

| Setting | Current default |
| --- | ---: |
| Recognition similarity threshold | 0.55 |
| Runner-up ambiguity margin | 0.10 |
| Unique person per frame | enabled |
| Auto-confirm above | 1.00 (disabled in practice) |
| Cluster edge threshold | 0.45 |
| Cluster merge threshold | 0.62 |

Older installs with exactly the original `0.42` threshold and `0.05` margin are
migrated in settings load to `0.55` / `0.10`; custom user values are retained.

Important recognition fixes in the current dirty tree:

1. **Clicking one face confirms only that face.** The previous implementation
   named every member of its machine-generated cluster. A single cluster error
   therefore contaminated both the group and the player library.
2. **Reference quality filtering and cap.** The highest-quality confirmed sample
   is always kept, additional references need quality >= 0.55, and no more than
   eight references per person are fed to the matcher.
3. **Named-cluster isolation.** Explicitly naming a cluster may confirm its
   members as a reviewed cluster, but `library_vectors` trusts only its cover
   face as a reusable reference. Individually tagged faces have no cluster and
   remain valid references.
4. **No runner-up reassignment bug.** In a group photo, a second face can no
   longer be assigned to its second- or third-ranked person merely because its
   best-ranked person was already used by another face. Every face must
   independently pass top-match threshold and margin.
5. **Stale suggestions are recomputed.** Every recognition pass clears only
   `suggested` assignments and recalculates them under current thresholds.
   Human `confirmed` decisions remain untouched.
6. **Video matching is frame-aware.** Faces are grouped by `(media_id,
   frame_time)` before applying the one-person-per-frame rule, allowing the same
   player to be recognized at several timestamps in one video.

After naming a face, the application immediately reruns recognition for the
shoot, regenerates albums, creates/reuses the person's editor group, and adds
the current person album's media to that group.

Manual group membership is persistent and intentionally never removed by AI
regeneration. Consequently, files already written into a contaminated group by
an older build do not disappear automatically. Clear that affected group once
and tag the correct face again; new matches use the stricter rules. A future
schema could track `manual` versus `ai` membership separately if automatic
pruning is desired without risking editor decisions.

## 8. Best-shot and duplicate ranking

The V1.2 foundation is implemented:

- Local sharpness and exposure scores are calculated from thumbnails.
- A perceptual photo hash groups near-duplicates.
- The highest-quality member becomes the suggested best shot.
- Album grids show quality, duplicate-count, and best-shot badges.
- Albums can filter best picks/duplicate groups and sort by quality.
- RAW and ordinary photos use the same ranking path.

Not complete:

- face-aware ranking (face size, occlusion, eyes open, expression);
- manual best-shot override;
- splitting/merging duplicate groups;
- accuracy/threshold validation on representative esports shoots.

## 9. Video sampling, tagging, and 4K resource controls

Videos are sampled rather than analyzed frame by frame. The planner combines
scene cuts with a fixed cadence, includes the opening frame, deduplicates nearby
timestamps, and caps sample count. Current defaults are a five-second interval,
60-frame cap, and 1280 px maximum decoded frame edge for analysis.

For sources above 2560x1440 (including 4K), the full scene-change scan is
skipped because it requires reading the whole high-bitrate video before sample
analysis. These videos use predictable interval sampling instead.

FFmpeg resource controls in the dirty tree:

- decode threads capped at 2;
- filter threads capped at 1;
- Windows processes run without a console and at below-normal priority;
- hardware decode is attempted first and falls back to capped software decode;
- sample output is uncompressed RGB PPM over a pipe, avoiding PNG encode/decode;
- frames are downscaled before face detection;
- only one sampled RGB frame is decoded/analyzed at a time unless the optional
  tracker keeps the immediately previous downscaled frame.

Sample timestamps are persisted in `video_sample_frames`. The media protocol
can render a review image for a selected timestamp. The frontend displays those
sample frames, draws/tag faces, allows manual boxes, and passes `frame_time`
through every command. A named face updates linked `video_detections`, and the
complete video is placed in the person's album/group.

## 10. Optional OpenCV-assisted video tracking

OpenCV was evaluated as a helper, not as a replacement for the established
recognition models. The accepted architecture is:

```text
FFmpeg decode/downscale
    -> SCRFD primary face detection
    -> OpenCV motion/feature proposal for detector misses
    -> fresh ArcFace embedding and identity verification
    -> persist only verified supplemental faces
```

Implementation details in the current dirty tree:

- Cargo feature: `opencv-tracking`.
- Rust API: `crates/video-analysis/src/tracking.rs`.
- Native bridge: `crates/video-analysis/native/opencv_tracking.cpp`.
- Build integration: `crates/video-analysis/build.rs`.
- OpenCV methods: strict pyramidal Lucas-Kanade optical flow with forward/back
  consistency, plus ORB feature matching and RANSAC similarity transform for
  longer gaps.
- SCRFD remains authoritative. OpenCV only fills a detector-missed box.
- A proposed crop receives a fresh ArcFace embedding and must match the source
  embedding at >= 0.58 before it can be persisted.
- Existing detector results are never replaced by a track.
- Uncertain tracks return no proposal and safely fall back to detector-only.
- Only the previous downscaled frame is retained, so tracking does not keep an
  entire video in memory.
- Settings reports either `OpenCV tracking` or `Detector only`.

OpenCV is project-local and ignored by Git. `scripts/setup-opencv.ps1`
downloads the official Windows OpenCV 4.13.0 SDK and verifies SHA-256
`F0E98C302464D6860777A7015065E11B9B271B5394E6BA92663F0CF1FC303F2C`.
It links `opencv_world4130.lib` and stages `opencv_world4130.dll`. This avoids
requiring a system-wide administrator install or LLVM/opencv-rust dependency.

Commands:

```powershell
npm run opencv:setup
npm run dev:opencv
npm run build:opencv
npm run rs:test:opencv
```

Real 4K calibration used a 3840x2160 network video named
`UB Vedio3183.MP4`. On a five-second frame jump the tracker deliberately
returned no proposal; SCRFD had already found the face. This proves safe
fallback, but also shows that tracking isolated five-second samples adds limited
recall. The next useful experiment is low-resolution intermediate tracking
frames between user-visible samples. Keep this bounded so it does not recreate
the original 100% CPU problem.

## 11. Processing order, performance, and recovery

The ordering bug for rotated files was fixed by making per-file AI analysis wait
until thumbnail/metadata indexing has completed. Shoot-wide recognition,
clustering, and albums also wait until all scan/thumbnail/analyze jobs finish.

Current resource policy:

- at most two background workers;
- one AI worker/model pair;
- per-session inference threads capped at four and divided across workers;
- at least one logical CPU reserved for UI/supporting work;
- models load lazily and unload after 30 seconds without AI work;
- progress events roughly every 500 ms;
- scan/database writes committed in batches of 200;
- blocked systemic dependencies (models/FFmpeg) requeue without burning retry
  counts and notices are throttled.

GPU provider policy:

- Windows: DirectML when available, CPU fallback.
- macOS: CoreML when available, CPU fallback.
- CUDA: optional feature.
- GPU ArcFace uses batch size 1 because DirectML/static model shapes reject the
  previous batch of 8. CPU may use batches up to 8.

Historical local benchmark on an RTX 3070 Ti, release profile:

| Operation | CPU | DirectML |
| --- | ---: | ---: |
| SCRFD detection | about 92 ms/image | about 36 ms/image |
| ArcFace embedding, batch 1 | about 45 ms/face | about 5.9 ms/face |

These are machine-specific measurements, not general accuracy/performance
guarantees.

## 12. Clear/reset behavior

The application supports:

- deleting one shoot index without touching source media;
- clearing scanned data for selected shoots;
- clearing all scanned indexes;
- clearing thumbnail cache;
- deleting one person's recognition data;
- deleting all face embeddings;
- clearing all recognition data;
- resetting/reanalyzing one shoot while retaining its media index;
- clearing an editor group without deleting source files.

The selected-shoot clear operation cancels those jobs, removes database-derived
data and related thumbnails only for those shoots, and leaves unselected shoots,
settings, models, and all source files alone.

## 13. UI areas and current capabilities

- **Shoots:** create/open/resume/delete indexes, processing summaries, selected
  scanned-data cleanup.
- **Sort into Groups:** the main editor workflow; manual groups, group chips,
  grouped/ungrouped counts, filter/sort, media preview, face tagging/manual face
  drawing, best-shot/quality visibility.
- **Players:** create, rename, team/notes, merge profiles, delete profile or only
  its recognition data.
- **AI Albums:** player, multi-player, optional team, unknown, and group-size
  albums; photo/video filtering and quality/best-shot controls.
- **Review:** suggestions/unknown/confirmed/everything, accept/reject/assign,
  mark not-a-face, bulk operations, open source media.
- **Copy & Organise:** export editor groups by default or AI albums optionally;
  Photos/Videos subfolders, conflict policy, timestamp preservation, progress,
  cancellation and history.
- **Settings:** acceleration, workers, analysis size, recognition thresholds,
  clustering, video sampling, FFmpeg path/status, OpenCV backend status, cache
  and privacy/data deletion.

Media is served to the WebView through the ID-based `teomedia://` custom
protocol. The frontend does not receive broad filesystem access. Video serving
supports bounded byte ranges; review sample images use a validated timestamp
query.

## 14. Build, run, test, and artifact locations

Prerequisites:

- Node.js 20+
- Rust stable/MSVC on Windows
- FFmpeg on `PATH` or configured in Settings for video and HEIC/HEIF/AVIF
- ONNX models in the app/project model location for recognition
- WebView2 on Windows (normally installed with Windows)

Common commands from the repository root:

```powershell
npm install
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1
npm run dev
npm run dev:opencv
npm run build
npm run build:opencv
npm run typecheck
npm run web:build
npm run rs:check
npm run rs:test
npm run rs:clippy
```

Always build the production desktop application through Tauri (`npm run build`
or the workspace Tauri build command). A plain `cargo build --release` can leave
Tauri in development URL mode and produce the `localhost refused to connect`
window seen earlier.

Expected build locations:

- frontend bundle: `apps/desktop/dist/`
- development executable: `target/debug/teo-desktop.exe`
- release executable: `target/release/teo-desktop.exe`
- Windows NSIS installer: `target/release/bundle/nsis/`
- macOS app/DMG when built on macOS: under `target/release/bundle/`

At the last inspection there was a current frontend bundle and debug executable,
but no current release/NSIS installer in this working tree. Do not tell the user
an installer exists until the bundle directory has been checked after a
successful Tauri release build.

Windows application data on this machine is normally:

```text
C:\Users\CG\AppData\Roaming\com.teorganiser.desktop\
  database\media.db
  thumbnails\
  face_cache\
  models\
  logs\teo.log
```

The production database observed during 4K calibration was
`C:\Users\CG\AppData\Roaming\com.teorganiser.desktop\database\media.db`.
Back it up before any manual SQL/data repair.

## 15. Most recent verification status

The latest uncommitted recognition tightening was verified on 3 September 2026:

- `teo-clustering`: 31 tests passed.
- `teo-database`: 59 tests passed after the P0 editorial migration/query tests.
- `teo-desktop`: 65 tests passed after the low-resolution hover-proxy route test.
- `teo-media-core`: 32 tests discovered (31 normal passes and one opt-in real
  GStreamer test), covering full-video proxy policy and isolated proxy storage.
- `teo-desktop --features opencv-tracking`: 64 tests passed.
- OpenCV/video-analysis native suite: 15 tests passed during its implementation.
- Clippy passed for clustering, database, desktop and the OpenCV feature work
  with warnings denied.
- TypeScript typecheck passed.
- Vite production frontend build passed (102 modules).
- `git diff --check` reported only expected LF-to-CRLF warnings, not whitespace
  errors.

Tests use in-memory SQLite and synthetic vectors/images where possible. They do
not replace a representative, consented face-recognition accuracy evaluation.

## 16. V1.2.0 requested scope and status

The user selected these V1.2 themes:

1. Best-shot and duplicate-photo ranking.
2. Visual face naming directly on photographs.
3. Explainable match evidence and reference comparisons.
4. A dashboard showing processing time and manual work saved.
5. Faster keyboard-based reviewing.
6. Release installers, backups, accuracy evaluation, and identified correctness
   fixes.

Status:

| Theme | Status |
| --- | --- |
| Best-shot/duplicate foundation | Implemented; advanced face-aware signals and overrides remain |
| Visual face naming | Implemented for photos and video samples; focused manual-box landmarks and undo remain |
| Explainable evidence | Not implemented beyond stored/displayed similarity and reference labeling |
| Outcome dashboard | Not implemented |
| Keyboard-first review | P0 implemented: persistent stars/Pick/Reject, bulk and viewer shortcuts, filters/sort; undo and help overlay remain |
| Resource-safe grid video preview | P0 implemented: delayed YouTube-style hover activation; one active player; complete GStreamer-generated H.264/AAC proxy at 512px width with source FPS/aspect/audio; one-second seek GOP and MP4 fast-start; serialized, thread-bounded import generation into the separate app-data `proxies` folder; viewer and hover use the proxy; poster retained until playback is ready; timeline above filename |
| Frame-aware video correctness | Implemented in dirty tree |
| Review/timeline synchronization | Partially implemented; audit every review operation |
| 4K resource caps | Implemented in dirty tree; real throughput measurement still needed |
| Selected-shoot scanned-data clearing | Implemented and committed |
| Database backup/restore | Not implemented |
| Accuracy evaluation | Not implemented |
| Signed Windows/macOS release pipeline | Not implemented in this checkout |

## 17. Recommended next engineering order

Preserve the user's earlier requested repair order and finish correctness before
adding novelty:

1. Commit the current tested dirty-tree work in coherent commits after reviewing
   the combined diff. Do not mix generated `.opencv`, `.scratch`, `target`, or
   private media/database files into Git.
2. Add database snapshot/restore before future migrations and before repairing
   existing recognition data.
3. Implement focused landmark detection inside manual face boxes, then compare
   aligned embeddings against the current bounding-box fallback.
4. Add an explicit “recalculate recognition and rebuild AI group” operation.
   If automatic group pruning is required, first add membership provenance
   (`manual` versus `ai`) so editor-added files are never silently removed.
5. Reanalyze the known affected rotated photographs only after backup.
6. Build a consented evaluation set split by lighting, motion blur, profile,
   occlusion, glasses, camera and demographic coverage. Report false-match and
   miss rates for 0.55/0.10 and nearby thresholds.
7. Prototype low-resolution intermediate video tracking frames and measure CPU,
   decode time and recovered detector misses before enabling it by default.
8. Finish explainable evidence/reference comparison in Review.
9. Build the outcome dashboard and finish keyboard review undo/help.
10. Add repeatable Windows and Apple Silicon release/signing/notarization gates.

## 18. V2.0 direction (not current implementation)

V2.0 is envisioned as a team media platform: NAS/server mode, remote CPU/GPU
workers, multi-user review, live collaboration, organisation player libraries,
editor/DAM integrations, compound search, and operational observability.

Do not begin this distributed architecture until V1.2 has repeatable installers,
database recovery, accuracy evidence, and real editor usage measurements. The
local/offline privacy-first mode must remain supported.

## 19. Documentation warnings

Use sources in this order:

1. Current source code and tests.
2. This `memory.md` snapshot.
3. `README.md`, `docs/current-application.md`, `docs/raw-support.md`,
   `docs/v1.2-progress.md`, and `docs/roadmap.md`.
4. Historical plans/work logs only for rationale.

Known stale/conflicting documentation as of this snapshot:

- `docs/current-application.md` and `docs/development.md` still list recognition
  defaults as 0.42/0.05. Current source is 0.55/0.10.
- `docs/current-application.md` says naming a cluster adds every cluster member
  as a reusable reference. Current library queries admit only the cluster cover;
  a direct face-tag action confirms only the clicked face.
- `docs/v1.2-progress.md` still labels frame-aware video recognition as queued;
  it is implemented and tested in the dirty tree.
- `docs/deployment.md`, parts of `docs/work-log-2026-08.md`, and
  `docs/server-architecture.md` describe a server-extraction branch with
  `teo-server`, sidecars, transports and CI files that do not exist in the
  current `windowsV2` checkout.
- The line in `media-core/src/ffmpeg.rs` describing FFmpeg still decode mentions
  camera RAW historically; actual routing sends supported camera RAW to LibRaw.

When future work changes behavior, update this memory and the canonical current
documentation together so this file does not become another stale plan.

## 20. Practical handoff checklist for Claude

Before editing:

- Run `git status --short` and preserve every existing change.
- Confirm the active branch remains `windowsV2` unless the user requests a new
  branch.
- Read the target module and its repository/tests before modifying behavior.
- Check `crates/database/src/migrations.rs`; migration 7 is already allocated
  to persistent editorial ratings and Pick/Reject state.
- Keep file operations safe for UNC/network paths and source media read-only.
- Keep OpenCV optional; the normal build must work without `.opencv`.
- Keep AI suggestions reviewable and never overwrite `confirmed` decisions.
- For videos, preserve `frame_time` through detection, matching, review,
  timeline synchronization and group generation.
- For RAW, use the central router and LibRaw; never add extension checks in
  random callers or route RAW back through FFmpeg.
- Test default and OpenCV feature builds when touching shared video code.
- Report honestly whether a change is committed/pushed, merely tested locally,
  or only planned.
