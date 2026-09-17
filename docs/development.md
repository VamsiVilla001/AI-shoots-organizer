# Development notes

## First run

The index lives in PostgreSQL 15+, so a server has to exist before the app or
the tests can do anything. Once per machine:

```bash
# 1. A server. Any PostgreSQL 15+ build with ICU support will do.
winget install PostgreSQL.PostgreSQL.17      # Windows
brew install postgresql@17 && brew services start postgresql@17   # macOS
sudo apt install postgresql                                       # Debian/Ubuntu

# 2. The `skwad` role, the `skwad` and `skwad_test` databases, and the
#    password file. Idempotent; never touches an existing database's contents.
PGPASSWORD=<postgres superuser password> npm run db:setup

# 3. Confirm.
npm run db:check
```

`npm run dev` then finds the server through `database.json` in the library
folder, falling back to `skwad` on `localhost:5432`. `SKWAD_DATABASE_URL`
overrides everything, which is the quickest way to point at a scratch database:

```bash
SKWAD_DATABASE_URL=postgres://skwad@localhost:5432/scratch npm run dev
```

### Bringing an old SQLite library across

Libraries from v2.0.0-alpha.3 and earlier are a `media.db` file. `skwad-db-migrate`
copies one into PostgreSQL, opening the source **read-only** so a failed run
costs nothing:

```bash
npm run db:migrate -- --sqlite "<path>/media.db" \
                      --postgres postgres://skwad@localhost:5432/skwad --dry-run
npm run db:migrate -- --sqlite "<path>/media.db" \
                      --postgres postgres://skwad@localhost:5432/skwad
npm run db:verify -- "<library folder>"     # reads it back through the app's own queries
```

`--dry-run` reads and counts everything, then rolls back. The real run is one
transaction that verifies per-table row counts before it commits, so a mismatch
leaves an empty database rather than half a library. Back up `media.db`
**together with its `-wal` and `-shm` siblings** first — a WAL database is all
three files, and copying only the `.db` silently loses whatever is still in the
log.

Ids are preserved deliberately: `thumbnail_path`, the sharded
`face_cache/<id>.jpg` crops and the `skwadmedia://` URLs the webview holds all
embed a media or face id, so renumbering would orphan every cached file on disk.

## Architecture at a glance

```
React UI  ──invoke──▶  Tauri commands (commands.rs)   ── thin: validate, query, return
   ▲                        │
   │ events                 ▼
   └────────────  PostgreSQL (skwad-database)  ◀── jobs table = the queue
                            ▲
        worker threads (worker.rs) ── claim job → run stage → write results
                            │
      ┌─────────────────────┼──────────────────────┐
      ▼                     ▼                      ▼
 skwad-media-core      face-detection +        skwad-clustering
 (scan/thumbs)       face-recognition        (match + cluster)
                     (ONNX Runtime)
```

Key decisions, and where to look:

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
- **The queue lives in the database** (`repo/jobs.rs`). `claim_next` is a single
  `UPDATE … RETURNING` whose sub-select ends `FOR UPDATE SKIP LOCKED`, so
  concurrent workers cannot double-claim. The locking clause is not optional
  now that writers really do run at the same time — SQLite serialised them, so
  the same statement was atomic without it. On startup `requeue_stale` recovers
  anything a crash left `running`.
- **One dedicated AI worker and one optional I/O helper** (`worker.rs`). Worker
  zero owns a single `pipeline.rs::Engine` and the shoot-wide finishing stages;
  worker one handles scans and thumbnails. This overlaps indexing with GPU
  inference without loading several detector/embedder pairs. Models load lazily,
  unload after 30 idle seconds, and rebuild when `AppState::settings_version`
  changes, so settings apply without restart.
- **Media reaches the webview through `skwadmedia://`** (`protocol.rs`), which
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

- `cargo test --workspace` — 350+ unit tests. **They need a PostgreSQL server**,
  which is the one thing the SQLite build did not: `:memory:` has no equivalent.
  Each test instead creates a uniquely-named schema on the test database and
  pins its pool's `search_path` to it, which preserves the property that
  mattered — two tests running at once cannot see each other's rows. Run
  `npm run db:setup` once; `SKWAD_TEST_DATABASE_URL` overrides the default of
  `postgres://postgres:postgres@localhost:5432/skwad_test`. Everything else
  stays hermetic (temp dirs; no models or network).
  - Test pools are deliberately `max_size(2)` with `min_idle(0)`. `cargo test`
    runs one pool *per test in parallel*, so the real ceiling is
    `max_size × test threads` against the server's `max_connections`; a pool of
    four exhausted a 36-core machine outright.
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
   `skwad-*` crate explicitly.
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
4. **Threads and model sessions were oversubscribed.** Four workers used to
   construct four detector/embedder pairs and could request more inference
   threads than the machine had. The current policy caps the pool at two,
   reserves one logical CPU for the UI/supporting work, gives only worker zero
   an AI engine, and caps inference threads at four.
5. **Long writer transactions look like a frozen application.** Scan inserts and
   job enqueues are committed in batches of 200 so UI reads and progress updates
   regularly regain access. This mattered more under SQLite, where one writer
   locked out every reader; Postgres readers are never blocked by a writer, so
   the batching now bounds transaction size and replication lag rather than
   preventing a freeze. It is kept because the batch size is also what keeps
   memory flat on a 50 000-file import.

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

Append a new entry to `MIGRATIONS` in `crates/database/src/migrations.rs` with
its SQL under `crates/database/src/sql/` — never edit an existing one; installed
databases have already run it. Versions are tracked in a `schema_migrations`
table (SQLite used `PRAGMA user_version`); the table also records *when* each
ran, which is the first thing you want when two libraries behave differently.
Check the highest version in use before claiming a number — a collision means
the migration is silently skipped on every database past that version.

Dialect traps, all of which compile fine and fail (or silently misbehave) at
runtime, because the compiler cannot see inside a SQL string:

- `?1` is not a placeholder. Postgres uses `$1`.
- `MAX(a, b)` / `MIN(a, b)` are `GREATEST` / `LEAST`. In Postgres `MAX` is only
  ever the aggregate, so the two-argument form is a "function does not exist"
  error rather than a wrong answer — but only when that branch runs.
- `LIKE` is case-sensitive. SQLite's ignored ASCII case, so a filename search
  needs `ILIKE` to keep behaving.
- A parameter needs a type. `$1 IS NULL`, `COALESCE($1, col)` and a bare `$1` in
  an `INSERT … SELECT` list give Postgres nothing to infer from; write
  `$1::bigint` / `$1::text`.
- `SELECT DISTINCT` may only `ORDER BY` expressions in its select list, which
  rules out `(x IS NULL)` and `x COLLATE nocase`. Group by the primary key
  instead — Postgres knows the rest of the row depends on it.
- Text ordering is locale-aware, where SQLite compared bytes. Name
  `COLLATE nocase` explicitly on any user-visible ordering so a studio server
  and a laptop sort a shoot identically.

## Folder-name previews

`apps/desktop/src/folders.ts` mirrors `export-engine/src/naming.rs` so the UI
can show the exact folder a group will produce before the export runs. The Rust
side is the authority; if the two disagree, fix the TypeScript.
