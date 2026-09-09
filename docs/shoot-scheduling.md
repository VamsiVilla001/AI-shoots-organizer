# Independent shoot scheduling

The desktop runs one indexing worker and two AI workers by default (one AI
worker on a single-core machine). Settings > Parallel AI workers accepts 1–10,
bounded by available CPU cores. Each AI worker owns a detector/embedder pair.
Model construction remains process-wide serialised for DirectML stability;
inference runs concurrently on separate sessions. Inactive worker slots do not
load models. Changes take effect between files without restarting the app.

Workers can analyse different videos from one shoot simultaneously. With
multiple ready shoots, the scheduler first favours shoots with fewer running
analyses, then rotates using one shared compute cursor. The indexing worker
has its own cursor. Thus GDR and Quarter Finals can have active analysis jobs
at the same time. Claims retain within-shoot stage priority and FIFO order
within a priority; completion order may differ because files take different
amounts of time. The same media cannot have two concurrently running jobs.

Pause in a shoot's progress panel affects only that shoot. No new jobs are
claimed for it; active jobs finish and persist their results. The panel shows
“Pausing — finishing active files” until they complete. Resume affects only
that shoot. Pause state and scheduling cursors are session-local. The global
pause flag is retained only for application maintenance operations. Cancel
remains scoped to the selected shoot. No active file is forcibly interrupted.

Atomic SQLite claims check readiness and pause exclusions before charging an
attempt. Metadata must be available before AI runs. Recognition, clustering
and albums wait for every media analysis in their own shoot, and execute
exclusively and in stage order for that shoot. They do not wait for other
shoots. Scheduler locking covers only claiming jobs, never decoding or AI.

Each extra AI worker consumes additional CPU, RAM and GPU memory. More workers
are not a promise of linear speedup. Inference thread budgets are divided
across AI workers. The old `workerThreads` setting is retained for JSON
compatibility; `aiWorkers` now controls AI concurrency. Recognition settings,
sampling coverage and stored vectors are unchanged. There is no local staging,
prefetch, database migration or automatic result deletion. Restart with the
updated build to activate the new worker-pool implementation for the first time.

Tests cover concurrent videos within one shoot, duplicate-media exclusion,
finishing barriers, four simultaneous claims spread across two shoots,
per-shoot pause/resume, legacy settings defaults, concurrency bounds and the
existing round-robin, retry, cancellation and metadata-dependency cases.
