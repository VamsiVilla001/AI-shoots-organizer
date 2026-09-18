//! Where a worker gets its jobs from, and where it reports back.
//!
//! The worker loop in [`crate::worker`] does not know whether it is the
//! server's own pool or a client that volunteered its GPU. It claims through
//! a [`JobSource`], heartbeats through it, and settles through it; the stages
//! underneath keep polling an `AtomicBool` for cancellation and never learn
//! what is setting it — which is the test that this seam is cut in the right
//! place.
//!
//! * [`LocalJobSource`] is the server's (and the desktop's) own pool: it talks
//!   to PostgreSQL directly and writes artifacts straight into the library
//!   folder it owns. No HTTP, no self-upload over loopback.
//! * A remote source is a client worker talking to `/api/work/*`; its
//!   cancellation flags are set by its heartbeat thread from the server's
//!   answer. Clients hold no database credentials, so worker traffic goes
//!   through the same front door as everything else.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use skwad_database::models::{Job, JobState};
use skwad_database::repo::jobs::{self, WorkerLane};

use crate::settings::AppSettings;
use crate::state::AppState;

/// What the queue told a worker when it tried to settle a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Recorded.
    Done,
    /// The lease was no longer held: reaped, cancelled or finished by a newer
    /// holder. Whatever the worker computed must be discarded.
    LeaseLost,
}

/// A worker's view of the queue.
pub trait JobSource: Send + Sync {
    /// The id every lease this worker takes is recorded under.
    fn machine_id(&self) -> &str;

    /// Claims the next job in `lane`, or `None` when there is nothing to do
    /// for this worker right now — including while processing is paused.
    fn claim(&self, lane: WorkerLane) -> skwad_database::Result<Option<Job>>;

    /// Starts keeping the lease alive. Called right after a claim.
    fn hold(&self, job: &Job);

    /// Stops keeping the lease alive. Called once the job is settled or
    /// abandoned.
    fn drop_lease(&self, job_id: i64);

    /// Returns a job without charging it an attempt — it cannot run *yet*.
    fn release(&self, job: &Job) -> Settled;

    fn complete(&self, job: &Job) -> Settled;

    /// `None` when the lease was lost; otherwise the state the job landed in.
    fn fail(&self, job: &Job, error: &str) -> Option<JobState>;

    /// The flag stages poll to stop early. The same `Arc<AtomicBool>` type
    /// `stages.rs` already takes, whoever sets it.
    fn cancel_flag(&self, shoot_id: i64) -> Arc<AtomicBool>;

    fn is_cancelled(&self, shoot_id: i64) -> bool;

    fn is_paused(&self) -> bool;

    fn is_shoot_paused(&self, shoot_id: i64) -> bool;

    /// Drops the remaining queued jobs of a cancelled shoot.
    fn cancel_shoot_jobs(&self, shoot_id: i64);

    /// The settings every worker must agree on, plus this machine's own.
    fn settings(&self) -> AppSettings;

    /// Changes when [`Self::settings`] would answer differently.
    fn settings_version(&self) -> u64;

    /// Records why a job could not run on this worker (missing models,
    /// missing FFmpeg), so the queue can explain itself.
    fn record_blockage(&self, shoot_id: i64, kind: &str, reason: &str);
}

// --- the local source ------------------------------------------------------------

/// The jobs this process currently holds a lease on, keyed by job id with the
/// fencing token each claim issued.
///
/// The lease keeper thread heartbeats every entry on its own schedule, so a
/// job's duration never interacts with the lease TTL: a twenty-minute video
/// analysis is heartbeated throughout without the worker thread doing
/// anything. A heartbeat that comes back [`jobs::Heartbeat::Cancelled`] or
/// [`jobs::Heartbeat::LeaseLost`] sets the shoot's cancellation flag, which is
/// how the stage running on the worker thread finds out.
#[derive(Default)]
pub struct HeldLeases {
    held: parking_lot::Mutex<HashMap<i64, HeldLease>>,
}

struct HeldLease {
    token: String,
    shoot_id: i64,
}

impl HeldLeases {
    pub fn hold(&self, job: &Job) {
        if let Some(token) = job.token() {
            self.held.lock().insert(
                job.id,
                HeldLease {
                    token: token.to_string(),
                    shoot_id: job.shoot_id,
                },
            );
        }
    }

    pub fn drop_lease(&self, job_id: i64) {
        self.held.lock().remove(&job_id);
    }

    pub fn snapshot(&self) -> Vec<(i64, String, i64)> {
        self.held
            .lock()
            .iter()
            .map(|(id, lease)| (*id, lease.token.clone(), lease.shoot_id))
            .collect()
    }

    pub fn count(&self) -> usize {
        self.held.lock().len()
    }
}

/// The server's (and desktop's) own pool: straight to the database.
pub struct LocalJobSource {
    state: Arc<AppState>,
    leases: Arc<HeldLeases>,
}

impl LocalJobSource {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            leases: Arc::new(HeldLeases::default()),
        }
    }

    pub fn leases(&self) -> &Arc<HeldLeases> {
        &self.leases
    }

    /// The token a claimed job carries. A job without one cannot have come
    /// from a claim, so this is an invariant violation rather than a runtime
    /// condition.
    fn token_of(job: &Job) -> &str {
        job.token().unwrap_or_default()
    }

    /// Heartbeats every lease this process holds. Runs on its own thread so
    /// job duration and heartbeat cadence are independent.
    pub fn keeper_loop(state: Arc<AppState>, leases: Arc<HeldLeases>) {
        let idle = std::time::Duration::from_millis(300);
        let mut last_beat = Instant::now();
        while !state.is_shutting_down() {
            std::thread::sleep(idle);
            if last_beat.elapsed() < jobs::HEARTBEAT_INTERVAL {
                continue;
            }
            last_beat = Instant::now();
            let held = leases.snapshot();
            if held.is_empty() {
                continue;
            }
            let Ok(mut conn) = state.db.conn() else { continue };
            for (job_id, token, shoot_id) in held {
                match jobs::heartbeat(&mut conn, job_id, &token) {
                    Ok(jobs::Heartbeat::Alive { .. }) => {}
                    Ok(jobs::Heartbeat::Cancelled) => {
                        tracing::info!(job = job_id, shoot = shoot_id, "job cancelled while running; stopping it");
                        state.cancel_shoot(shoot_id);
                        leases.drop_lease(job_id);
                    }
                    Ok(jobs::Heartbeat::LeaseLost) => {
                        // Someone else may hold this job now. The stage is
                        // told to stop via the shoot flag; its results are
                        // refused by the token gate regardless.
                        tracing::warn!(job = job_id, shoot = shoot_id, "lease lost while running; abandoning the job");
                        state.cancel_shoot(shoot_id);
                        leases.drop_lease(job_id);
                    }
                    Err(error) => tracing::warn!(job = job_id, %error, "could not heartbeat a lease"),
                }
            }
        }
    }
}

impl JobSource for LocalJobSource {
    fn machine_id(&self) -> &str {
        &self.state.machine_id
    }

    fn claim(&self, lane: WorkerLane) -> skwad_database::Result<Option<Job>> {
        let mut conn = self.state.db.conn()?;
        self.state.claim_job(&mut conn, lane)
    }

    fn hold(&self, job: &Job) {
        self.leases.hold(job);
    }

    fn drop_lease(&self, job_id: i64) {
        self.leases.drop_lease(job_id);
    }

    fn release(&self, job: &Job) -> Settled {
        let outcome = self
            .state
            .db
            .conn()
            .and_then(|mut conn| jobs::release(&mut conn, job.id, Self::token_of(job)));
        self.leases.drop_lease(job.id);
        match outcome {
            Ok(true) => Settled::Done,
            Ok(false) => Settled::LeaseLost,
            Err(error) => {
                tracing::warn!(job = job.id, %error, "could not release a job");
                Settled::LeaseLost
            }
        }
    }

    fn complete(&self, job: &Job) -> Settled {
        let outcome = self
            .state
            .db
            .conn()
            .and_then(|mut conn| jobs::complete(&mut conn, job.id, Self::token_of(job)));
        self.leases.drop_lease(job.id);
        match outcome {
            Ok(true) => Settled::Done,
            Ok(false) => Settled::LeaseLost,
            Err(error) => {
                tracing::warn!(job = job.id, %error, "could not mark a job done");
                Settled::LeaseLost
            }
        }
    }

    fn fail(&self, job: &Job, error: &str) -> Option<JobState> {
        let outcome = self
            .state
            .db
            .conn()
            .and_then(|mut conn| jobs::fail(&mut conn, job.id, Self::token_of(job), error));
        self.leases.drop_lease(job.id);
        match outcome {
            Ok(state) => state,
            Err(db_error) => {
                tracing::warn!(job = job.id, error = %db_error, "could not record a job failure");
                Some(JobState::Failed)
            }
        }
    }

    fn cancel_flag(&self, shoot_id: i64) -> Arc<AtomicBool> {
        self.state.cancellation(shoot_id)
    }

    fn is_cancelled(&self, shoot_id: i64) -> bool {
        self.state.is_cancelled(shoot_id)
    }

    fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    fn is_shoot_paused(&self, shoot_id: i64) -> bool {
        self.state.is_shoot_paused(shoot_id)
    }

    fn cancel_shoot_jobs(&self, shoot_id: i64) {
        if let Ok(mut conn) = self.state.db.conn() {
            let _ = jobs::cancel_for_shoot(&mut conn, shoot_id);
        }
    }

    fn settings(&self) -> AppSettings {
        self.state.settings()
    }

    fn settings_version(&self) -> u64 {
        self.state.settings_version()
    }

    fn record_blockage(&self, shoot_id: i64, kind: &str, reason: &str) {
        self.state.record_blockage(shoot_id, kind, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skwad_database::models::JobKind;
    use skwad_database::repo::shoots;
    use skwad_database::Database;

    fn state() -> Arc<AppState> {
        let temp = std::env::temp_dir().join(format!("skwad-jobsource-{}", std::process::id()));
        let paths = crate::paths::AppPaths::create(&temp).unwrap();
        Arc::new(AppState::new(
            Database::open_test().unwrap(),
            paths,
            AppSettings::default(),
            "skwadmedia://".into(),
            "local-test",
            temp.join(format!("machine-{}.json", uuid::Uuid::new_v4())),
        ))
    }

    /// The local source is the reference the remote one is tested against:
    /// claim, hold, settle, and the lease bookkeeping around it.
    #[test]
    fn the_local_source_claims_holds_and_settles_through_the_lease() {
        let state = state();
        let source = LocalJobSource::new(Arc::clone(&state));
        {
            let mut conn = state.db.conn().unwrap();
            let shoot = shoots::create(&mut conn, "S", "S").unwrap();
            jobs::enqueue(&mut conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();
            jobs::enqueue(&mut conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();
        }

        let first = source.claim(WorkerLane::Compute).unwrap().unwrap();
        assert_eq!(first.owner.as_deref(), Some("local-test"));
        source.hold(&first);
        assert_eq!(source.leases().count(), 1);
        assert_eq!(source.release(&first), Settled::Done);
        assert_eq!(source.leases().count(), 0, "a released job is no longer heartbeated");

        let again = source.claim(WorkerLane::Compute).unwrap().unwrap();
        source.hold(&again);
        assert_eq!(source.complete(&again), Settled::Done);
        assert_eq!(source.complete(&again), Settled::LeaseLost, "settling twice is refused");

        let last = source.claim(WorkerLane::Compute).unwrap().unwrap();
        source.hold(&last);
        assert_eq!(source.fail(&last, "boom"), Some(JobState::Queued));
        assert_eq!(source.leases().count(), 0);

        state.set_paused(true);
        assert!(source.claim(WorkerLane::Compute).unwrap().is_none(), "paused means nothing to do");
    }

    #[test]
    fn held_leases_track_only_jobs_in_flight() {
        let state = state();
        let mut conn = state.db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        jobs::enqueue(&mut conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();
        let job = jobs::claim_next(&mut conn, None, "test").unwrap().unwrap();

        let leases = HeldLeases::default();
        leases.hold(&job);
        assert_eq!(leases.count(), 1);
        let (id, token, shoot_id) = leases.snapshot().remove(0);
        assert_eq!((id, shoot_id), (job.id, shoot.id));
        assert!(matches!(
            jobs::heartbeat(&mut conn, id, &token).unwrap(),
            jobs::Heartbeat::Alive { .. }
        ));

        leases.drop_lease(job.id);
        assert_eq!(leases.count(), 0);
        assert!(leases.snapshot().is_empty());
    }
}
