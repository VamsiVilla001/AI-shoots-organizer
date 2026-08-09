# Esports AI Media Organiser

A local-first desktop application that sorts esports photo/video shoots by
player. Import a shoot folder, let on-device face recognition find and group
every player, name each person once, review the uncertain matches, and export
the original files into player-wise folders.

> Reduce hours of manual player-image sorting into a short AI-assisted review
> workflow.

**Platforms:** Windows 10/11 · macOS (Apple Silicon)
**Stack:** Tauri 2 · React + TypeScript · Rust · ONNX Runtime · FFmpeg · SQLite

Everything runs locally. No cloud APIs, no uploads; original media is never
modified, moved or renamed.

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
  export-engine/         Player-wise folder export (copy, never move)
packages/shared-types/   TypeScript mirrors of every IPC type
models/                  ONNX models (fetched, not committed)
scripts/                 fetch-models, icon generation
docs/                    Architecture and development notes
```

## Getting started

Prerequisites: Rust (stable, MSVC on Windows), Node 20+, FFmpeg on `PATH`
(needed for videos, HEIC and camera raw — JPEG/PNG shoots work without it).

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

GPU acceleration is opt-in at compile time (the plain build uses CPU):

```bash
# Windows (DirectML)
npm run tauri:build -w @teo/desktop -- -- --features directml

# macOS (CoreML)
npm run tauri:build -w @teo/desktop -- -- --features coreml
```

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
   regenerated from face assignments at any time.
6. **Review** — accept/reject suggestions, bulk-assign, merge/split, mark
   false detections; corrections feed the library and improve the next shoot.
7. **Export** — originals are *copied* into `Player/Photos|Videos` folders
   with collision-safe names; the source folder is never written to.

Processing is a resumable SQLite-backed job queue — quitting mid-import loses
nothing. See [docs/development.md](docs/development.md) for more detail, and
the original plan in `docs/` for the product spec.
