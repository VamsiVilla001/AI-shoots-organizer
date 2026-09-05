# Esports AI Media Organiser

A local-first desktop application that sorts esports photo/video shoots into
per-person folders. Point it at a raw footage folder, let on-device face
recognition propose the grouping, name and correct the groups by hand, then
export the originals into a folder per group — on a NAS share or any other
destination.

**Current stable release:** 1.0.0

**Active development branch:** 1.2.0

> Reduce hours of manual footage sorting into a short review pass: name a group
> once in the app and every file you put in it lands in a folder of that name.

**Platforms:** Windows 10/11 · macOS (Apple Silicon)
**Stack:** Tauri 2 · React + TypeScript · Rust · ONNX Runtime · LibRaw · FFmpeg · GStreamer · optional OpenCV tracking · SQLite

Everything runs locally. No cloud APIs, no uploads; the source folder is only
ever read — every export copies originals into a new destination.

## Repository layout

```
apps/desktop/            Tauri app — React frontend (src/) + Rust shell (src-tauri/)
crates/
  database/              SQLite schema, migrations, repositories, job queue
  media-core/            Folder scanner, EXIF metadata, decoding, thumbnails
  face-detection/        ONNX Runtime setup, SCRFD detector, NMS
  face-recognition/      Landmark alignment, ArcFace embeddings
  clustering/            Player matching + unknown-face clustering
  video-analysis/        Scene detection and frame sampling over FFmpeg
  export-engine/         Group-wise folder export (copy, never move)
packages/shared-types/   TypeScript mirrors of every IPC type
models/                  ONNX models (fetched, not committed)
scripts/                 fetch-models, icon generation
docs/                    Architecture and development notes
```

## Getting started

Prerequisites: Rust (stable, MSVC on Windows), Node 20+, FFmpeg on `PATH`
(needed for video analysis and HEIC), and the GStreamer 1.0 runtime (needed for
full-duration 512px video proxies). Camera RAW is decoded by bundled LibRaw on
Windows.

```bash
npm install

# fetch the face models (~280 MB, one time)
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1   # Windows
bash scripts/fetch-models.sh                                        # macOS

# run in development
npm run dev

# package an installer
npm run build
```

To test the Windows OpenCV-assisted video path, download its project-local SDK
and launch the feature build. The normal command above remains the
detector-only fallback and does not require OpenCV.

```powershell
npm run dev:opencv
```

OpenCV optical-flow and feature tracking only propose missed face locations between sampled video
frames. SCRFD still detects faces and ArcFace must verify every proposed crop
before it is stored, so tracking cannot directly assign a player's identity.

GPU acceleration is on by default: DirectML on Windows and CoreML on macOS are
operating-system components, and ONNX Runtime falls back to the CPU provider
when neither can start. Measured on an RTX 3070 Ti, that is ~2.6x on detection
and ~7.9x on embedding. Force a provider from **Settings → Acceleration**.

NVIDIA CUDA is opt-in, since it needs a toolkit installed separately:

```bash
npm run tauri:build -w @teo/desktop -- -- --features cuda
```

If a shoot is analysing slowly, check that you are running a **release** build
(`npm run build`) — `npm run dev` compiles for debugging and is several times
slower.

## Verifying the code

```bash
npm run rs:test        # Rust workspace tests
npm run rs:clippy      # lints
npm run typecheck      # TypeScript
```

## How it works

1. **Scan** — the shoot folder is walked and indexed into SQLite; thumbnails
   generate in the background so the grid is browsable immediately.
2. **Detect + embed** — SCRFD finds faces, each is aligned via its landmarks
   and embedded with an ArcFace model into a 512-d vector (batched per image).
3. **Recognise** — embeddings are compared against the player library built
   from every face a human has confirmed; confident matches become
   *suggestions* (nothing is final without review).
4. **Cluster** — unmatched faces are grouped with Chinese-Whispers label
   propagation; each group is one "Unknown Person" to name once.
5. **Albums** — player, multi-player, team and unidentified albums are
   regenerated from face assignments at any time. A second, independent axis
   groups every file by *how many* people are in it — `Single`, `Two persons`,
   … `10+ persons` — so solo portraits and full team shots are one click apart.
6. **Review** — accept/reject suggestions, bulk-assign, merge/split, mark
   false detections; corrections feed the library and improve the next shoot.
7. **Sort** — the editor's own layer: create a named group per person (or per
   anything — "Team B-roll", "Day 2 Interviews"), drag or bulk-assign files
   into it, and see at a glance what is still unsorted. One click seeds the
   groups from the AI players so you correct instead of sorting from scratch,
   and manual grouping survives a re-analysis untouched.
8. **Export** — originals are *copied* into one folder per group
   (`Group/Photos|Videos`) with collision-safe names, plus a report of what
   went where; the source folder is never written to. Exporting the AI albums
   directly is still one toggle away.

Face recognition is optional to the sorting flow: with no models installed the
shoot still scans, and every group can be filled by hand.

Processing is a resumable SQLite-backed job queue — quitting mid-import loses
nothing. See the [current full application documentation](docs/current-application.md)
for the implemented product guide, [docs/development.md](docs/development.md)
for engineering notes, [docs/release-1.0.md](docs/release-1.0.md) for the
noteworthy 1.0 feature inventory, and [docs/roadmap.md](docs/roadmap.md) for the
1.2 and 2.0 product plan. Active 1.2 work is tracked in
[docs/v1.2-progress.md](docs/v1.2-progress.md). The original plan in `docs/`
remains the historical product specification.
