# Development notes

## Architecture at a glance

```
React UI  ──invoke──▶  Tauri commands (commands.rs)   ── thin: validate, query, return
   ▲                        │
   │ events                 ▼
   └──────────────  SQLite (teo-database)  ◀── jobs table = the queue
                            ▲
        worker threads (worker.rs) ── claim job → run stage → write results
                            │
      ┌─────────────────────┼──────────────────────┐
      ▼                     ▼                      ▼
 teo-media-core      face-detection +        teo-clustering
 (scan/thumbs)       face-recognition        (match + cluster)
                     (ONNX Runtime)
```

Key decisions, and where to look:

- **Everything derived is rebuildable.** Albums, clusters and suggestions can
  be regenerated from `faces` at any time; human decisions (`assignment =
  'confirmed'`, named clusters) are never overwritten by a re-run. See
  `crates/database/src/repo/albums.rs::regenerate` and `stages.rs`.
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

## The type contract

Rust structs in `crates/database/src/models.rs` serialise camelCase and are
mirrored by hand in `packages/shared-types/src/index.ts`. When one changes,
change the other in the same commit.

## Testing

- `cargo test --workspace` — 180+ unit tests, all hermetic (in-memory SQLite,
  temp dirs; no models or network needed).
- AI correctness is pinned by math-level tests: SCRFD anchor decode,
  similarity-transform alignment, cosine/kNN, cluster determinism (seeded
  PRNG in `cluster.rs` — clustering the same shoot twice gives identical
  results on purpose).
- Anything touching ONNX Runtime for real needs model files, and is exercised
  manually via the app; keep new logic on the pure side of that line where
  possible.

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
existing one; installed databases have already run it.
