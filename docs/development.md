# Development notes

## Architecture at a glance

```
React UI  ──invoke──▶  Tauri commands (commands.rs)   ── thin: validate, query, return
   ▲                        │
   │ events                 ▼
   │                 teo-app-core  ── state · settings · worker · pipeline
   │                       │          stages · export · models · paths
   └── ProgressSink ───────┤
                           ▼
                    SQLite (teo-database)  ◀── jobs table = the queue
                           ▲
        worker threads (app-core/worker.rs) ── claim job → run stage → write results
                           │
      ┌────────────────────┼──────────────────────┐
      ▼                    ▼                      ▼
 teo-media-core      face-detection +        teo-clustering
 (scan/thumbs)       face-recognition        (match + cluster)
                     (ONNX Runtime)
```

Key decisions, and where to look:

- **The core knows nothing about its front door** (`crates/app-core`). It takes
  an `AppPaths` instead of asking a path resolver, and pushes events into a
  `ProgressSink` instead of emitting to a webview — `TauriProgressSink` in the
  desktop shell, `NullProgressSink`/`RecordingProgressSink` in tests. That seam
  is what lets a headless build reuse the pipeline rather than copy it; see
  [server-architecture.md](server-architecture.md).
- **Everything derived is rebuildable.** Albums, clusters and suggestions can
  be regenerated from `faces` at any time; human decisions (`assignment =
  'confirmed'`, named clusters) are never overwritten by a re-run. See
  `crates/database/src/repo/albums.rs::regenerate` and `stages.rs`.
- **Manual groups are not derived** (`repo/groups.rs`, §34). `media_groups` is
  the editor's own sorting and the default source of export folders, so nothing
  in the AI path may rewrite it: `stages::reset_analysis`,
  `clear_all_recognition_data` and `albums::regenerate` all leave it alone.
  Group names become folder names, which is why they are validated on the way
  in rather than at export time.
- **The queue lives in SQLite** (`repo/jobs.rs`). `claim_next` is a single
  `UPDATE … RETURNING`, so concurrent workers cannot double-claim. On startup
  `requeue_stale` recovers anything a crash left `running`.
- **One AI engine per worker thread** (`pipeline.rs::Engine`). ONNX sessions
  are not shared; models load once per worker, not once per file. A bump of
  `AppState::settings_version` makes workers rebuild their engines, so settings
  apply without restart.
- **Media reaches the webview through `teomedia://`** (`protocol.rs`), which
  resolves database ids — the webview never gets raw filesystem access. The
  `full/` route is also what makes HEIC/RAW previewable (decoded via FFmpeg).
- **Face crops in the UI are CSS crops** of the cached thumbnail using the
  normalised bounding box (`FaceCrop.tsx`) — no crop files to generate.
- **Bounding boxes are stored normalised (0..1)** so they are valid against
  any rendering size of the frame.
- **`media.face_count` is not a people count.** It counts face *rows*, and
  video analysis writes one row per detection **per sampled frame**, so a
  one-person interview sampled 20 times has `face_count = 20`. Group-size
  albums use `media.person_count`, maintained by
  `repo::media::refresh_person_counts`:

  ```text
  max( distinct identities + most unidentified faces in one frame,
       most faces visible in any one frame )
  ```

  Identity is `person_id`, falling back to `cluster_id`. The first term keeps a
  repeatedly-sampled player at 1; the middle term stops one unrecognised
  stranger counting once per frame; the floor exists because two faces in a
  single frame are two people, so a clustering mistake cannot drag the count
  below reality. One expression covers photos and video — a photo's
  `frame_time` is NULL, so all its faces form a single group.

  `albums::regenerate` refreshes these counts as its first step, which makes
  regeneration the single place they are guaranteed current: no review action
  has to remember to update them.

## The type contract

Rust structs in `crates/database/src/models.rs` serialise camelCase and are
mirrored by hand in `packages/shared-types/src/index.ts`. When one changes,
change the other in the same commit.

## Testing

- `cargo test --workspace` — 200+ unit tests, all hermetic (in-memory SQLite,
  temp dirs; no models or network needed).
- AI correctness is pinned by math-level tests: SCRFD anchor decode,
  similarity-transform alignment, cosine/kNN, cluster determinism (seeded
  PRNG in `cluster.rs` — clustering the same shoot twice gives identical
  results on purpose).
- Anything touching ONNX Runtime for real needs model files, and is exercised
  manually via the app; keep new logic on the pure side of that line where
  possible.

## Performance, and the traps in it

Measured on a 16-core machine with an RTX 3070 Ti, release profile, using the
bundled buffalo_l models:

| | CPU | DirectML |
| --- | --- | --- |
| Detection (fixed 640×640) | 92 ms/image | **36 ms/image** |
| Embedding, batch 1 | 45 ms/face | **5.9 ms/face** |
| Embedding, batch 8 | 46 ms/face | *fails* — see below |

Four things this pinned down, each of which had a wrong default:

1. **Cargo's `[profile.dev.package."*"]` does not cover workspace members.**
   Our own crates were compiling at `opt-level = 0` during `npm run dev`:
   `align_face` 4.03 ms vs 0.77 ms, `cosine` 8.3 µs vs 0.42 µs, `knn_graph`
   over 1500 faces 2409 ms vs 72 ms. The root `Cargo.toml` now names each
   `teo-*` crate explicitly.
2. **DirectML requires static shapes.** Given a batch of 8 against a model
   whose graph declares a batch of 1, it does not degrade — it fails with
   `BatchNormalization … The parameter is incorrect`, failing every face.
   `ArcFaceEmbedder` therefore caps its batch at 1 on any GPU provider and
   chunks accordingly (`CPU_MAX_BATCH` / `GPU_MAX_BATCH`). Batching is worth
   only ~5% on CPU, so this costs almost nothing and buys a 7.9x speed-up.
3. **Benchmarks must validate their results.** An early measurement showed
   DirectML at "42x" because it was timing inferences that were erroring out.
   Anything measuring inference has to assert the embeddings come back and are
   unit length.
4. **Threads were oversubscribed.** `worker_threads` × `inference_threads` used
   to exceed the core count (4 × 8 on a 16-core machine); the default now
   divides the machine between workers instead of giving each one half of it.

## Threshold defaults

Tuned for ArcFace-family embeddings (same-person cosine typically > 0.5,
different-person < 0.3):

| Setting | Default | Where |
| --- | --- | --- |
| Detection score | 0.5 | `settings.rs` |
| Recognition threshold | 0.42 | " |
| Recognition margin | 0.05 | " |
| Cluster edge threshold | 0.45 | " |
| Cluster merge threshold | 0.62 | " |

All are user-configurable in Settings and clamped in
`AppSettings::sanitised`.

## Adding a new pipeline stage

1. Add a `JobKind` variant (`models.rs`) and a priority in `stages.rs`.
2. Implement the stage in `stages.rs` (idempotent, per-shoot) or
   `pipeline.rs` (per-file).
3. Route it in `worker.rs::run_job`.
4. Queue it from `scan_shoot` / `queue_pending_work`.

## Schema changes

Append a new migration to `crates/database/src/migrations.rs` — never edit an
existing one; installed databases have already run it. Migration 3
(`schema_003_groups.sql`) is the worked example: it adds tables and touches
nothing an earlier migration created. Check the highest version already in use
before claiming a number — a collision means the migration is silently skipped
on every database that has passed that version.

## Folder-name previews

`apps/desktop/src/folders.ts` mirrors `export-engine/src/naming.rs` so the UI
can show the exact folder a group will produce before the export runs. The Rust
side is the authority; if the two disagree, fix the TypeScript.
