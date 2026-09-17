# SKWAD Media Organiser — Architecture

**v2.0.0-alpha.3** · Tauri 2 · React/TS · Rust · ONNX Runtime · PostgreSQL
Local-first desktop app that sorts a shoot folder into per-person folders using
on-device face recognition. Originals are read-only; nothing leaves the machine
except an optional encrypted, metadata-only `.skwad` catalogue.

## Shape

```text
WebView (React)  ──invoke──▶  Tauri shell  ──▶  worker pool (1 I/O + N AI)
  api.ts (typed)  ◀──events──   commands.rs        │
  TanStack Query                state · pipeline   ▼
  zustand (UI only)             stages · export   8 domain crates
        │                                          (database, media-core,
        └──skwadmedia://──▶ protocol.rs            face-detection/-recognition,
                                                   clustering, video-analysis,
  Premiere UXP panel ──HTTP──▶ loopback :51823     export-engine, catalogue)
```

One long-lived process. Two optional companions: the Premiere panel (separate
Adobe process, polls us over loopback) and `services/backend` (axum; holds the
org signing key, run out-of-band, not on any critical path).

Domain crates are Tauri-free. The shell layer is Tauri-aware only where it
emits events — that's the seam the planned core extraction cuts (§Debt).

## The three invariants

1. **Non-destructive.** Source folders are opened read-only. Export writes
   native shortcuts (or copies) elsewhere and hard-refuses a destination inside
   the source (`ExportError::DestinationInsideSource`).
2. **Resumable.** Every unit of work is a row in the `jobs` table.
   `requeue_stale` returns orphaned `running` rows to `queued` at startup; quit
   mid-import loses nothing.
3. **Idempotent shoot stages.** `scan`, `recognise`, `cluster`, `albums`
   re-run to the same result and never undo a human decision. That's what makes
   "Resume" and "Re-analyse" safe to press blind.

## Pipeline

```text
scan → thumbnail → proxy → analysePhoto|analyseVideo → recognise → cluster → albums
      └── per file, I/O worker ──┘└── per file, AI ──┘└──── per shoot ─────┘
```

SCRFD detects (5 landmarks) → similarity-transform alignment → ArcFace 512-d →
cosine match against the confirmed-face library → Chinese-Whispers clustering
of the remainder. Suggestions are a distinct state from confirmations;
`auto_confirm_above` defaults to never.

Video is sampled (scene detection + interval, capped), not exhaustively
decoded. Optional OpenCV optical flow only *proposes* boxes — a proposal must
clear a flow-confidence bar **and** yield a fresh ArcFace vector similar to the
source face before it's stored. Tracking can never assign identity.

## Non-obvious decisions

| Decision | Why |
| --- | --- |
| Each AI worker owns its own detector+embedder `Engine` | ORT sessions need exclusive access to run; loading per-worker not per-file is the dominant saving |
| Engine *construction* serialised process-wide (`OnceLock<Mutex>`) | ORT's DirectML provider corrupts the native heap when several large sessions initialise concurrently. Execution stays parallel |
| Engines dropped after 30 s idle | Each pair pins hundreds of MB of model + GPU state |
| Custom `skwadmedia://` scheme; Tauri asset protocol **disabled** | Widening asset scope to arbitrary paths lets any loaded page read any file. This resolves a *media id* through the DB first, so the webview reaches only indexed files |
| `analysis_max_dim` downsize before detection | Single largest perf lever in the system |
| `opt-level = 3` named per workspace member in `Cargo.toml` | Cargo's `"*"` glob does **not** cover workspace members. Without it, dev builds measured 5x slower alignment, 20x slower cosine, 33x slower kNN |
| Premiere bridge: fixed port + fixed token | UXP manifests must allow-list an exact `domain:port` (no reliable wildcard). Token only distinguishes us from another local process — identity comes from the signed-in desktop session. Loopback-only, so a constant costs nothing and removes all panel setup |
| Send-to-Premiere is a queue, not a call | We can't invoke Premiere's scripting API; the panel drains `premiere_queue` on its poll |
| Panel installs at app launch, not from the installer | UXP installs per-user; the Windows bundle is `perMachine`, so an NSIS hook would install into the admin's profile, not the editor's. Also gets macOS free |
| `library.json` in per-machine app data | The library pointer can't live in the database — the database is the thing it locates. `SKWAD_LIBRARY_ROOT` overrides |
| Blockages (missing FFmpeg/models) in memory, 20 s TTL | A blockage is a fact about this run, not about the stored job |
| `settings_version: AtomicU64` | Workers rebuild inference sessions on bump — threshold/accelerator changes apply without restart |

## Data model

The index lives in **PostgreSQL**; the **library folder** holds everything that
is not rows — `database.json` (which server to talk to), `auth/credentials.json`,
`models/`, logs and caches. `cache_root` can stay machine-local, keeping bulky
rebuildable files off the network.

Sharing a library is now "point two machines at one server" rather than "put a
`.db` on a UNC path". That retires `StorageMode::NetworkShare`, which existed
only to downgrade SQLite's journal because WAL needs shared memory an SMB client
cannot provide; concurrency is the server's job now. It also means the app is no
longer zero-install — `Database::connect` fails loudly if no server answers,
rather than falling back.

Credentials never enter the library folder: `database.json` carries host, port,
database and user, and the password comes from `SKWAD_DATABASE_PASSWORD` or the
standard `pgpass` file. `SKWAD_DATABASE_URL` overrides the lot.

**Two organisation axes that never overwrite each other** — the central model
decision:

- **AI albums** are *derived* — regenerated from face assignments on demand.
- **Media groups** are *authored* — the editor names them and fills them.

`groups_from_ai_albums` seeds groups once so the editor corrects rather than
sorts from scratch. Re-analysis leaves authored groups untouched.

**Projects → Collections** sit above both: per-account, nestable, shareable
(private/invited/organisation). `project_collection_sources` stores only
`(shoot, group)` references — files stay owned by their shoot; a collection
copies nothing.

One `sql/001_baseline.sql`, tracked in a `schema_migrations` table. Repos in
`crates/database/src/repo/`. The thirteen SQLite migrations collapsed into that
baseline because no Postgres database ever ran them — an existing SQLite library
arrives through `skwad-db-migrate`, which copies rows into the finished schema.

Three dialect choices are worth knowing before touching a query:

| Choice | Why |
| --- | --- |
| `TEXT COLLATE nocase` (a non-deterministic ICU collation) for the columns that were `COLLATE NOCASE` | Gives case-insensitive `=` *and* `UNIQUE`. `citext` would have worked too, but it is a distinct type and rust-postgres refuses to send a `&str` for its OID, so every parameter would have needed a cast |
| Booleans stay `0`/`1` `BIGINT`, timestamps stay RFC3339 `TEXT` | The repo layer reads them as `i64`/`String` and the TS layer receives them unchanged. Switching to `BOOLEAN`/`timestamptz` would have rippled through the IPC boundary for no behavioural gain |
| Every claim in `repo::jobs` ends `FOR UPDATE SKIP LOCKED` | SQLite serialised writers, so `UPDATE … WHERE id = (SELECT … LIMIT 1)` was atomic for free. Postgres runs the workers concurrently: without it two workers pick the same row and one silently re-claims a running job |

## Frontend

TanStack Query owns everything from Rust; zustand owns only UI state (nav,
progress, notices, theme, clipboard). `api.ts` wraps every command and
`packages/shared-types` mirrors every IPC type, so a shape change in
`commands.rs` is a TS compile error. `eventBridge.ts` maps six pushed events to
the query keys they invalidate — during processing only cheap counters refresh;
expensive lists invalidate once at `complete`. Two shells (`projects` default,
`classic`) over identical commands.

## Security

- **Auth** — offline. Argon2id hashes in a versioned `credentials.json` in the
  library folder (so a shared library = shared accounts). Admin commands reject
  non-admins server-side; can't delete the last enabled admin or your own
  signed-in account.
- **`.skwad`** — metadata only, format v2: XChaCha20-Poly1305 payload, HPKE
  (X25519-HKDF-SHA256) per-recipient key wrap, Argon2id ≥64 MiB for password
  recipients, Ed25519 signature with domain separation, authenticated header,
  `PackageLimits` capping header/ciphertext/decompressed size before
  allocation, plus traversal-safe path normalisation.
- **Bridge** — loopback-only, read-only Collection data. See table above.
- **CSP** — `script-src 'self'`; `img-src`/`media-src` admit only our scheme.

## Degradation

Missing dependencies narrow features, never block: **no models** → scan, group
and export still work; **no FFmpeg** → JPEG/PNG only (no video/HEIC/RAW);
**no GStreamer** → no full-duration proxies; **no GPU** → CPU fallback (~2.6x
slower detect, ~7.9x slower embed); **no Creative Cloud** → panel install logs
and skips.

## Architectural debt — read before extending

1. **The core is not extracted.** `state`/`worker`/`pipeline`/`stages`/`export`
   still live in the Tauri crate: no headless build, no HTTP front door.
   [server-architecture.md](server-architecture.md) specifies the intended
   `skwad-app-core` + `skwad-server` split and the `ProgressSink` seam replacing
   `AppHandle::emit`. Phases 5–7 (NAS container, remote GPU worker, multi-user
   hardening) unstarted.
2. **[deployment.md](deployment.md) documents a `skwad-server` sidecar and
   `npm run package:win` that do not exist** in `tauri.conf.json` or
   `package.json`. Doc drift, not a missing build step.
3. **`commands.rs` is ~2,300 lines / ~170 commands** — the natural first split
   when the core moves.
4. **`ENFORCE_PASSWORD_CHANGE = false` with a seeded shared password**
   (`catalogue.rs`). Testing posture; flip before any rollout.
5. **The Premiere `.ccx` is unsigned.** Adobe's agent is the sanctioned route
   for internal plugins, but tolerance for unsigned packages has moved across
   Creative Cloud releases — verify on one machine before a wide rollout.
6. **`supabase/` is vestigial** — prototype migrations, unused by local auth.

---

Detail on any section: [architecture-plan.md](architecture-plan.md) (original
spec) · [current-application.md](current-application.md) (product guide) ·
[skwad-v2-security.md](skwad-v2-security.md) · [development.md](development.md)
