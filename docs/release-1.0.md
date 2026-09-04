# SKWAD Media Organiser 1.0.0

## Release position

Version 1.0.0 is the first stable product baseline for SKWAD Media Organiser. It is a
complete local desktop workflow for indexing an esports shoot, finding and
grouping people, reviewing identity decisions, and copying originals into
editor-ready folders.

The 1.0 label means the core workflow and local data model are established. It
does not claim that every platform build has been commercially distributed or
that face recognition is infallible. Windows is the primary validated target;
macOS signing, notarisation, and hardware acceptance remain release work.

## Noteworthy product features

### Local-first face intelligence

- SCRFD detects faces and five-point facial landmarks on the user's computer.
- ArcFace creates normalised identity embeddings through ONNX Runtime.
- Media, names, face crops, and embeddings are not sent to a cloud API.
- DirectML on Windows and CoreML on macOS are supported with CPU fallback;
  CUDA can be enabled as an opt-in build feature.
- Detection, matching, ambiguity, clustering, sampling, and acceleration
  settings remain user-configurable instead of being hidden constants.

### An editor-centred workflow

- A shoot folder is indexed in place; normal app operations never modify,
  rename, move, or delete an original.
- Unknown faces are grouped with deterministic Chinese-Whispers-style graph
  clustering, so a person can be named as a group instead of file by file.
- Naming a cluster confirms its faces and adds them to a reusable player
  library for later shoots.
- Recognition produces reviewable suggestions by default. A human can accept,
  reject, reassign, merge, split, or mark a detection as false.
- Human-confirmed decisions are protected from ordinary regeneration passes.

### Useful media organisation, not just face tagging

- Player albums contain every indexed file in which that player appears.
- Multi-player albums surface recurring pairs, while optional team albums
  collect players by team.
- An independent group-size axis creates No people, Single, Two persons,
  through 10+ persons albums.
- Person albums can be cross-filtered by group size, such as one player's solo
  photographs.
- Match confidence is shown separately from manual references; the UI does not
  invent an AI confidence for a human assignment.

### Photo, raw, and video handling

- Native JPEG, PNG, WebP, TIFF, and BMP support is supplemented by FFmpeg for
  HEIC, HEIF, AVIF, camera raw formats, and common video containers.
- EXIF orientation is applied before analysis so thumbnails, face boxes, and
  model input use the same coordinate system.
- Video analysis combines scene changes with interval samples, removes nearby
  duplicates, and caps work on long recordings.
- Video detections retain timestamps and are collapsed into appearance ranges
  for timeline navigation.
- Content-addressed, sharded thumbnails are reusable across rescans.

### Resilient long-running processing

- SQLite in WAL mode stores the media index, face library, settings, albums,
  job queue, export history, and audit log.
- The persistent job queue resumes interrupted work after restart, atomically
  claims jobs, retries transient failures, and isolates permanently bad files.
- Scanning and database insertion are batched to keep the interface responsive
  on large shoots.
- AI models load lazily, reload when settings change, unload when idle, and are
  restricted to one compute worker to avoid GPU-memory contention.
- Indexing and thumbnails continue to work even when AI models are not yet
  installed.

### Safe, practical export

- Export is previewed before it writes, including file count, byte count, and
  destination folders.
- Originals are copied into player-wise Photos and Videos folders.
- A destination equal to or inside the shoot source is rejected.
- Portable folder-name sanitisation handles illegal characters, reserved
  Windows names, length limits, and case-insensitive filename collisions.
- Re-runs can skip, rename, or overwrite existing files, preserve timestamps,
  report progress, support cancellation, and retain export history.

### Security and privacy details worth highlighting

- The webview receives media through an ID-based custom protocol rather than
  broad filesystem access.
- Video serving supports bounded byte ranges for seeking without exposing raw
  paths to the frontend.
- Users can delete one player's recognition data, every embedding, all
  recognition data, thumbnails, logs, or an entire shoot index independently.
- The audit log stores actions and identifiers, not embeddings or face crops.

## Engineering features worth discussing publicly

- Rust owns filesystem, database, media, AI, clustering, jobs, and export work;
  React and TypeScript provide the desktop workflow through Tauri.
- Detection and embedding models sit behind replaceable traits.
- Face boxes are normalised to the frame, making them valid against any render
  size. Review crops are produced with CSS instead of additional crop files.
- The clusterer is deterministic across runs and the k-nearest-neighbour graph
  stays O(n·k) in memory rather than materialising an O(n²) matrix.
- DirectML's static-batch limitation is handled explicitly: GPU embeddings use
  batch one while CPU embeddings can be chunked in batches of eight.
- Measured on an RTX 3070 Ti, the documented development benchmark records
  roughly 2.6× faster detection and 7.6× faster single-face embedding with
  DirectML than CPU on the tested models and machine. These are machine-specific
  engineering measurements, not universal performance guarantees.

## Honest 1.0 boundaries

- Face similarity is not a calibrated probability; every identity result can
  be wrong and must remain correctable.
- Model accuracy has not yet been published against a representative,
  consented esports benchmark split by lighting, pose, skin tone, and camera.
- macOS source paths exist, but a signed/notarised build still needs validation
  on Apple hardware.
- ONNX model weights and FFmpeg distribution need a polished first-run and
  licensing story for a one-click public installer.
- The current checkout is a local desktop architecture. Server/NAS and
  multi-user material elsewhere in `docs/` describes later work, not 1.0.

Those boundaries are why the next milestone focuses on trust, correctness,
packaging, and measurable editor outcomes rather than adding novelty alone.
