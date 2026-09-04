# SKWAD Media Organiser product roadmap

## Version strategy

- **1.0.0 — Local workflow baseline:** the current desktop product described
  in `release-1.0.md`.
- **1.0.x / 1.1.x — maintenance:** focused bug fixes, dependency updates, and
  installer corrections may ship without changing the product promise.
- **1.2.0 — Trusted editor release:** make the local workflow easier to ship,
  measure, correct, and demonstrate professionally.
- **2.0.0 — Team media platform:** introduce shared storage, remote compute,
  collaboration, and integrations. This is intentionally not folded into 1.2.

## 1.2.0 — Trusted editor release

### Release goal

An esports editor should be able to install SKWAD Media Organiser, process a real shoot,
understand why each result exists, correct mistakes quickly, and prove how much
sorting work was saved. Windows and Apple Silicon builds should have repeatable
release gates.

### P0: correctness and release hardening

1. **Make video recognition frame-aware.** Group embeddings by media and sampled
   `frame_time` before applying the one-person-per-frame rule. The current
   shoot recogniser groups all detections in a video as one frame, which can
   suppress later appearances of the same player. Add regression tests for a
   player appearing in several sampled frames and keep video timeline identity
   rows synchronised after every manual correction.
2. **Refresh derived state after every review action.** Confirm, reject,
   reassign, ignore, merge, and clear-recognition operations should update
   albums, person counts, timelines, and shoot summaries atomically or enqueue
   one explicit refresh job. The user should never need to press Regenerate to
   see the consequence of a correction.
3. **Separate export and processing cancellation.** Give each export its own
   cancellation token and record. Cancelling a copy must not cancel or resume
   the shoot's analysis state.
4. **Make rescans reconcile the source.** Detect files removed from or renamed
   inside the source folder, show a reconciliation summary, and remove stale
   index rows only after a clear confirmation policy.
5. **Create a repeatable release pipeline.** Add Windows and Apple Silicon CI,
   unit/type/lint gates, installer smoke tests, version consistency checks,
   signed Windows builds, macOS signing/notarisation, checksums, and release
   notes generated from the tag.
6. **Add database backup and migration recovery.** Snapshot `media.db` before a
   schema migration, expose backup/restore in Settings, and test upgrade paths
   using real older-version fixtures rather than fresh databases alone.

### P1: noteworthy editor features

1. **Visual name-on-photo workflow.** In the full-media viewer, let the editor
   click a face box, name that person, preview the cluster that will inherit
   the name, and then gather their complete album in one confirmed operation.
   Make the propagation scope visible before committing it.
2. **Best-shot and duplicate culling.** Add perceptual duplicate groups plus
   sharpness, face size, occlusion, eyes-open, and expression signals. Rank the
   best hero shot without deleting the alternatives. This expands the product
   from identity sorting into genuine editorial assistance.
3. **Explainable match evidence.** For a suggestion, show the best reference
   faces, similarity, runner-up margin, image quality, and whether the result
   came from a manual reference or cluster propagation. A reviewer should be
   able to understand uncertainty at a glance.
4. **Outcome dashboard.** Record media processed, review decisions, processing
   time, estimated manual clicks avoided, files gathered per named cluster,
   and export completion. Let the user export a privacy-safe run report. These
   numbers create the strongest product demo and LinkedIn story.
5. **Fast review ergonomics.** Add keyboard-first accept/reject/assign actions,
   selection ranges, undo/redo, saved filters, pagination/virtualisation, and
   a side-by-side source/reference comparison.
6. **Portable player-library packages.** Export and import an encrypted,
   consent-aware player library with model/version metadata, duplicate-person
   detection, and an explicit choice about whether embeddings or only profile
   metadata are included.

### P1: trust, privacy, and evaluation

1. **Consent and retention controls.** Add per-player consent/retention notes,
   an expiry review, a local-data inventory, and a one-click deletion receipt.
2. **Representative evaluation suite.** Build a consented test set covering
   stage lighting, motion blur, side profiles, partial occlusion, glasses,
   varied cameras, and demographic diversity. Report false-match and miss
   rates at each threshold instead of presenting cosine similarity as
   accuracy.
3. **Privacy threat model.** Document local attack surfaces, model provenance,
   data at rest, logs, temporary files, backups, and deletion guarantees.
   Consider optional OS-backed encryption for the database and embeddings.
4. **Model and dependency notices.** Surface model source, hash, version,
   license, and active provider inside the app. Verify downloaded artifacts
   before installing them.

### 1.2.0 acceptance criteria

- A clean Windows machine can install, process a photo/video fixture, review a
  match, and export without developer tools.
- A signed/notarised Apple Silicon build completes the same fixture on real
  hardware.
- A repeated-player video regression proves that recognition is frame-aware.
- Every review action is reflected in albums and timelines without manual
  regeneration.
- Cancelling export leaves processing state unchanged.
- Upgrade tests open a backed-up 1.0 database and preserve human decisions.
- A documented, consented evaluation report publishes precision/recall-style
  identity metrics at the shipped default thresholds.
- The app can produce a run report showing time, throughput, review effort,
  and output without exposing biometric data.

## 2.0.0 — Team media platform

### Release goal

Move from one workstation and one editor to a production media team working
from shared storage, while preserving the local-first option and the invariant
that source media is never silently modified.

### Major product capabilities

1. **NAS/server mode.** Run the core as a service beside shared media and let
   desktop or browser clients connect without mounting every path locally.
2. **Remote and heterogeneous workers.** Schedule scanning, CPU work, and GPU
   inference separately; advertise worker capabilities; survive worker loss;
   and make every job idempotent.
3. **Multi-user review.** Add accounts, roles, shoot ownership, assignment
   queues, optimistic locking, comments, and a decision history that records
   who confirmed each identity.
4. **Live collaboration.** Push shared progress and review changes, prevent two
   editors from unknowingly resolving the same face, and support handoff from
   ingest to reviewer to exporter.
5. **Production integrations.** Export manifests, bins, markers, or metadata
   for Adobe Premiere Pro, DaVinci Resolve, Final Cut Pro, and common DAM
   systems. Keep ordinary folder export as the universal fallback.
6. **Organisation-level player library.** Support teams, aliases, roster
   periods, transfers, jersey seasons, consent state, duplicates, and scoped
   sharing between projects.
7. **Search beyond identity.** Combine player, team, group size, timecode,
   camera metadata, shoot, quality, and optional semantic/event tags such as
   trophy lift, interview, stage entrance, or gameplay reaction.
8. **Operational observability.** Provide queue health, throughput, GPU use,
   error-rate trends, storage growth, audit exports, backup state, and a clear
   health page without collecting customer media.

### 2.0 architecture gates

- One application core must serve desktop and network transports without
  duplicating business rules.
- Authentication, authorisation, path jailing, TLS or trusted-network policy,
  secrets, and tenant boundaries must be designed before remote access ships.
- A shared deployment must never expose an arbitrary filesystem browser or raw
  source path outside the configured media and output roots.
- Database and job-queue choices must be backed by load tests representative
  of concurrent shoots and workers.
- Offline/local desktop mode remains supported; 2.0 must not turn privacy into
  a cloud dependency.

## Recommended order of work

Start with the six 1.2 P0 items. Then build the outcome dashboard and visual
naming workflow because they make the existing intelligence easier to trust
and demonstrate. Best-shot culling is the strongest new editorial feature once
the correctness and release gates are in place. Begin 2.0 only after 1.2 has a
repeatable installer, evaluation dataset, migration safety, and real editor
usage data; otherwise distributed architecture will amplify unresolved local
workflow problems.
