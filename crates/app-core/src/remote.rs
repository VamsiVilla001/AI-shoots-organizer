//! A client's view of the server's queue: [`RemoteJobSource`].
//!
//! Everything [`crate::job_source::LocalJobSource`] does against PostgreSQL,
//! this does against `/api/work/*` with a machine token. The worker loop
//! cannot tell them apart, which is the point: a laptop that volunteers its
//! GPU runs the same `Engine::compute` on the same files and the server
//! applies the result exactly as it would its own.
//!
//! What is different is *where things are*:
//!
//! * The media row and the job arrive together in the claim, because there
//!   is no database to look them up in.
//! * The bytes are read from the share when the shoot has a mapping and the
//!   path exists on this machine, and downloaded otherwise.
//! * The models must be the server's pair, by content hash. [`RemoteJobSource::sync_models`]
//!   fetches any that are missing before the first claim.
//! * Cancellation and lost leases arrive on the heartbeat, which runs on its
//!   own thread and sets the same `AtomicBool` the stages already poll.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Serialize;
use skwad_database::models::{Job, JobState, Media};
use skwad_database::repo::jobs::{self, WorkerLane};
use skwad_database::repo::machines::Capabilities;
use skwad_database::DbError;

use crate::analysis::AnalysisOutput;
use crate::job_source::{HeldLeases, JobSource, Settled, WorkError};
use crate::models::{hash_file, ModelRegistry, ModelRole};
use crate::paths::AppPaths;
use crate::settings::{AppSettings, LibrarySettings, MachineSettings};
use crate::work_api::*;

/// How a worker reaches its server.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// `https://server:8787`, no trailing slash needed.
    pub base_url: String,
    pub machine_token: String,
    /// This installation's id — the `owner` the server writes into leases.
    pub machine_id: String,
}

/// What the worker-mode panel shows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub connected: bool,
    pub paused: bool,
    pub last_error: Option<String>,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    /// Jobs this machine holds a lease on right now.
    pub held: usize,
    pub library_version: u64,
    pub models_ready: bool,
}

/// Downloads land here, under the library-less client's own data folder,
/// and are removed once the job that needed them is settled.
const DOWNLOAD_DIR: &str = "downloads";

/// Per-request ceiling. Uploads of a review frame and downloads of one
/// original both stream, so this only bounds a stalled connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// A claim that keeps being refused (models differ, machine revoked) is
/// reported this often rather than on every poll.
const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum RemoteError {
    /// The server could not be reached or did not answer in time.
    Transport(String),
    /// The server answered, and said no.
    Status(StatusCode, String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteError::Transport(message) => write!(f, "cannot reach the server: {message}"),
            RemoteError::Status(status, message) => write!(f, "{message} ({status})"),
        }
    }
}

impl From<RemoteError> for WorkError {
    fn from(error: RemoteError) -> Self {
        match error {
            // The file is fine; this machine cannot talk to the server.
            RemoteError::Transport(_) => WorkError::Blocked(error.to_string()),
            RemoteError::Status(status, message) if status.is_server_error() => {
                WorkError::Blocked(format!("the server could not take the result: {message}"))
            }
            RemoteError::Status(_, message) => WorkError::Failed(message),
        }
    }
}

struct ClaimedWork {
    media: Media,
    client_path: Option<String>,
    /// A downloaded copy, deleted once the job is settled.
    download: Option<PathBuf>,
}

pub struct RemoteJobSource {
    http: Client,
    base: String,
    machine_token: String,
    machine_id: String,
    paths: AppPaths,
    machine: RwLock<MachineSettings>,
    library: RwLock<LibrarySettings>,
    library_version: AtomicU64,
    /// Bumped whenever [`JobSource::settings`] would answer differently.
    settings_version: AtomicU64,
    leases: Arc<HeldLeases>,
    claimed: Mutex<HashMap<i64, ClaimedWork>>,
    cancel_flags: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    /// What the result call said, handed to the `complete` that follows.
    settled: Mutex<HashMap<i64, Settled>>,
    /// A blockage recorded for a shoot, sent with the release that follows.
    blockages: Mutex<HashMap<i64, String>>,
    paused: AtomicBool,
    shutdown: Arc<AtomicBool>,
    status: Mutex<RemoteStatus>,
    last_refusal_log: Mutex<Option<Instant>>,
}

impl RemoteJobSource {
    /// Connects and fetches the library settings. Fails when the server is
    /// unreachable or the token is not accepted, with a message a person can
    /// act on.
    pub fn connect(config: RemoteConfig, paths: AppPaths, machine: MachineSettings) -> Result<Arc<Self>, String> {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("could not build an HTTP client: {error}"))?;
        let source = Arc::new(Self {
            http,
            base: config.base_url.trim_end_matches('/').to_string(),
            machine_token: config.machine_token,
            machine_id: config.machine_id,
            paths,
            machine: RwLock::new(machine),
            library: RwLock::new(LibrarySettings::default()),
            library_version: AtomicU64::new(0),
            settings_version: AtomicU64::new(1),
            leases: Arc::new(HeldLeases::default()),
            claimed: Mutex::new(HashMap::new()),
            cancel_flags: Mutex::new(HashMap::new()),
            settled: Mutex::new(HashMap::new()),
            blockages: Mutex::new(HashMap::new()),
            paused: AtomicBool::new(false),
            shutdown: Arc::new(AtomicBool::new(false)),
            status: Mutex::new(RemoteStatus::default()),
            last_refusal_log: Mutex::new(None),
        });
        source.fetch_library().map_err(|error| error.to_string())?;
        source.status.lock().connected = true;
        Ok(source)
    }

    pub fn leases(&self) -> &Arc<HeldLeases> {
        &self.leases
    }

    pub fn status(&self) -> RemoteStatus {
        let mut status = self.status.lock().clone();
        status.held = self.leases.count();
        status.paused = self.paused.load(Ordering::Relaxed);
        status.library_version = self.library_version.load(Ordering::Relaxed);
        status
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Replaces this machine's half of the settings — its worker count, its
    /// accelerator. Workers rebuild their engines on the next job.
    pub fn set_machine_settings(&self, machine: MachineSettings) {
        *self.machine.write() = machine;
        self.settings_version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Runs the lease keeper on its own thread until [`Self::shutdown`].
    pub fn start_keeper(self: &Arc<Self>) -> std::thread::JoinHandle<()> {
        let source = Arc::clone(self);
        std::thread::Builder::new()
            .name("skwad-remote-keeper".into())
            .spawn(move || source.keeper_loop())
            .expect("failed to spawn the remote lease keeper")
    }

    // --- HTTP plumbing ------------------------------------------------------

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn authed(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header(MACHINE_TOKEN_HEADER, &self.machine_token)
            .header("x-skwad-api", API_VERSION.to_string())
    }

    fn send(&self, request: RequestBuilder) -> Result<Response, RemoteError> {
        let response = self
            .authed(request)
            .send()
            .map_err(|error| RemoteError::Transport(error.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let message = response
            .json::<serde_json::Value>()
            .ok()
            .and_then(|value| value.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| format!("the server answered {status}"));
        Err(RemoteError::Status(status, message))
    }

    fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R, RemoteError> {
        self.send(self.http.get(self.url(path)))?
            .json()
            .map_err(|error| RemoteError::Transport(format!("unexpected answer: {error}")))
    }

    fn post_json<B: Serialize, R: DeserializeOwned>(&self, path: &str, body: &B) -> Result<R, RemoteError> {
        self.send(self.http.post(self.url(path)).json(body))?
            .json()
            .map_err(|error| RemoteError::Transport(format!("unexpected answer: {error}")))
    }

    fn note_error(&self, error: &RemoteError) {
        let mut status = self.status.lock();
        status.connected = !matches!(error, RemoteError::Transport(_));
        status.last_error = Some(error.to_string());
    }

    fn note_ok(&self) {
        let mut status = self.status.lock();
        status.connected = true;
        status.last_error = None;
    }

    // --- settings and models -------------------------------------------------

    fn fetch_library(&self) -> Result<SettingsResponse, RemoteError> {
        let response: SettingsResponse = self.get_json("/api/work/settings")?;
        *self.library.write() = response.settings.clone();
        self.library_version.store(response.library_version, Ordering::Relaxed);
        self.settings_version.fetch_add(1, Ordering::Relaxed);
        Ok(response)
    }

    /// Refetches the library settings when the server says they changed.
    fn observe_library_version(&self, version: u64) {
        if version != self.library_version.load(Ordering::Relaxed) {
            match self.fetch_library() {
                Ok(_) => tracing::info!(version, "library settings changed; workers will reload"),
                Err(error) => tracing::warn!(%error, "could not refetch the library settings"),
            }
        }
    }

    /// Makes sure this machine has the server's model pair and points its own
    /// settings at it. Downloads what is missing, verifying each file's hash.
    pub fn sync_models(&self) -> Result<(), String> {
        let list: ModelsResponse = self.get_json("/api/models").map_err(|error| error.to_string())?;
        let registry = ModelRegistry::new(&self.paths.models);
        let mut machine = self.machine.read().clone();
        for model in &list.models {
            let file_name = match registry.find_by_hash(&model.hash) {
                Some(local) => Path::new(&local.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| model.name.clone()),
                None => {
                    self.download_model(model)?;
                    model.name.clone()
                }
            };
            match model.role {
                ModelRole::Detector => machine.detector_model = Some(file_name),
                ModelRole::Embedder => machine.embedder_model = Some(file_name),
                ModelRole::Unknown => {}
            }
        }
        let ready = list.models.iter().any(|m| m.role == ModelRole::Detector)
            && list.models.iter().any(|m| m.role == ModelRole::Embedder);
        self.status.lock().models_ready = ready;
        self.set_machine_settings(machine);
        if ready {
            Ok(())
        } else {
            Err("the server has no complete model pair installed".into())
        }
    }

    fn download_model(&self, model: &crate::models::ModelInfo) -> Result<(), String> {
        std::fs::create_dir_all(&self.paths.models).map_err(|error| format!("create models folder: {error}"))?;
        let target = self.paths.models.join(&model.name);
        let temporary = self.paths.models.join(format!("{}.part", model.name));
        tracing::info!(model = %model.name, bytes = model.size_bytes, "downloading a model from the server");
        let mut response = self
            .send(self.http.get(self.url(&format!("/api/models/{}", model.hash))))
            .map_err(|error| format!("download {}: {error}", model.name))?;
        {
            let mut file = std::fs::File::create(&temporary).map_err(|error| format!("write {}: {error}", temporary.display()))?;
            std::io::copy(&mut response, &mut file).map_err(|error| format!("download {}: {error}", model.name))?;
            file.flush().map_err(|error| error.to_string())?;
        }
        let hash = hash_file(&temporary).map_err(|error| error.to_string())?;
        if hash != model.hash {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("the downloaded copy of {} did not match the server's hash", model.name));
        }
        std::fs::rename(&temporary, &target).map_err(|error| format!("place {}: {error}", target.display()))?;
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        let machine = self.machine.read().clone();
        let status = ModelRegistry::new(&self.paths.models)
            .status(machine.detector_model.as_deref(), machine.embedder_model.as_deref());
        Capabilities {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            gpu: Some(format!("{:?}", machine.accelerator)),
            detector_hash: status.detector_hash,
            embedder_hash: status.embedder_hash,
            ai_workers: machine.ai_workers,
            on_battery: false,
        }
    }

    // --- leases ---------------------------------------------------------------

    fn token_of(job: &Job) -> &str {
        job.token().unwrap_or_default()
    }

    fn job_path(job_id: i64, tail: &str) -> String {
        format!("/api/work/{job_id}/{tail}")
    }

    fn set_cancelled(&self, shoot_id: i64) {
        self.cancel_flag(shoot_id).store(true, Ordering::Relaxed);
    }

    /// Heartbeats every held lease on the heartbeat cadence, and learns about
    /// cancellations and settings changes from the answers.
    fn keeper_loop(&self) {
        let idle = Duration::from_millis(300);
        let mut last_beat = Instant::now();
        while !self.shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(idle);
            if last_beat.elapsed() < jobs::HEARTBEAT_INTERVAL {
                continue;
            }
            last_beat = Instant::now();
            for (job_id, token, shoot_id) in self.leases.snapshot() {
                let outcome: Result<HeartbeatResponse, RemoteError> =
                    self.post_json(&Self::job_path(job_id, "heartbeat"), &HeartbeatRequest { token });
                match outcome {
                    Ok(answer) => {
                        self.note_ok();
                        self.observe_library_version(answer.library_version);
                        match answer.status {
                            HeartbeatStatus::Alive => {}
                            HeartbeatStatus::Cancelled => {
                                tracing::info!(job = job_id, shoot = shoot_id, "job cancelled while running; stopping it");
                                self.set_cancelled(shoot_id);
                                self.leases.drop_lease(job_id);
                            }
                            HeartbeatStatus::LeaseLost => {
                                tracing::warn!(job = job_id, shoot = shoot_id, "lease lost while running; abandoning the job");
                                self.set_cancelled(shoot_id);
                                self.leases.drop_lease(job_id);
                            }
                        }
                    }
                    Err(error) => {
                        self.note_error(&error);
                        tracing::warn!(job = job_id, %error, "could not heartbeat a lease");
                    }
                }
            }
        }
    }

    fn download_original(&self, job: &Job, media: &Media) -> Result<PathBuf, WorkError> {
        let directory = self.paths.root.join(DOWNLOAD_DIR);
        std::fs::create_dir_all(&directory)
            .map_err(|error| WorkError::Blocked(format!("cannot create the download folder: {error}")))?;
        let target = directory.join(format!("{}-{}", job.id, media.filename));
        let mut response = self
            .send(
                self.http
                    .get(self.url(&Self::job_path(job.id, "original")))
                    .header(LEASE_TOKEN_HEADER, Self::token_of(job)),
            )
            .map_err(WorkError::from)?;
        let mut file = std::fs::File::create(&target)
            .map_err(|error| WorkError::Blocked(format!("cannot write {}: {error}", target.display())))?;
        std::io::copy(&mut response, &mut file)
            .map_err(|error| WorkError::Blocked(format!("download of {} failed: {error}", media.filename)))?;
        Ok(target)
    }

    fn forget(&self, job_id: i64) {
        if let Some(work) = self.claimed.lock().remove(&job_id) {
            if let Some(download) = work.download {
                let _ = std::fs::remove_file(download);
            }
        }
    }
}

impl JobSource for RemoteJobSource {
    fn machine_id(&self) -> &str {
        &self.machine_id
    }

    fn claim(&self, lane: WorkerLane) -> skwad_database::Result<Option<Job>> {
        if lane != WorkerLane::Remote || self.is_paused() {
            return Ok(None);
        }
        let request = ClaimRequest {
            capabilities: self.capabilities(),
        };
        match self.post_json::<_, ClaimResponse>("/api/work/claim", &request) {
            Ok(answer) => {
                self.note_ok();
                self.observe_library_version(answer.library_version);
                let Some(claimed) = answer.job else { return Ok(None) };
                let job = claimed.job;
                self.claimed.lock().insert(
                    job.id,
                    ClaimedWork {
                        media: claimed.media,
                        client_path: claimed.client_path,
                        download: None,
                    },
                );
                Ok(Some(job))
            }
            // A refusal — models differ, machine revoked, server paused — is
            // not an error to retry in a tight loop. Say so occasionally.
            Err(error @ RemoteError::Status(_, _)) => {
                self.note_error(&error);
                let mut last = self.last_refusal_log.lock();
                if last.is_none_or(|at| at.elapsed() >= REFUSAL_LOG_INTERVAL) {
                    *last = Some(Instant::now());
                    tracing::warn!(%error, "the server is not handing this machine work");
                }
                Ok(None)
            }
            Err(error) => {
                self.note_error(&error);
                Err(DbError::other(error.to_string()))
            }
        }
    }

    fn hold(&self, job: &Job) {
        self.leases.hold(job);
    }

    fn drop_lease(&self, job_id: i64) {
        self.leases.drop_lease(job_id);
        self.forget(job_id);
    }

    fn release(&self, job: &Job) -> Settled {
        let blocked = self.blockages.lock().remove(&job.shoot_id);
        let request = ReleaseRequest {
            token: Self::token_of(job).to_string(),
            blocked,
        };
        let outcome: Result<SettleResponse, RemoteError> = self.post_json(&Self::job_path(job.id, "release"), &request);
        self.drop_lease(job.id);
        match outcome {
            Ok(answer) if answer.settled => Settled::Done,
            Ok(_) => Settled::LeaseLost,
            Err(error) => {
                self.note_error(&error);
                tracing::warn!(job = job.id, %error, "could not release a job");
                Settled::LeaseLost
            }
        }
    }

    fn complete(&self, job: &Job) -> Settled {
        // The result call settled the job on the server; this only reports
        // what it said.
        let settled = self.settled.lock().remove(&job.id).unwrap_or(Settled::LeaseLost);
        self.drop_lease(job.id);
        if settled == Settled::Done {
            self.status.lock().jobs_completed += 1;
        }
        settled
    }

    fn fail(&self, job: &Job, error: &str) -> Option<JobState> {
        let request = FailRequest {
            token: Self::token_of(job).to_string(),
            error: error.to_string(),
        };
        let outcome: Result<FailResponse, RemoteError> = self.post_json(&Self::job_path(job.id, "fail"), &request);
        self.drop_lease(job.id);
        self.status.lock().jobs_failed += 1;
        match outcome {
            Ok(answer) => answer.state,
            Err(remote) => {
                self.note_error(&remote);
                tracing::warn!(job = job.id, error = %remote, "could not record a job failure");
                None
            }
        }
    }

    fn cancel_flag(&self, shoot_id: i64) -> Arc<AtomicBool> {
        Arc::clone(
            self.cancel_flags
                .lock()
                .entry(shoot_id)
                .or_insert_with(|| Arc::new(AtomicBool::new(false))),
        )
    }

    fn is_cancelled(&self, shoot_id: i64) -> bool {
        self.cancel_flags
            .lock()
            .get(&shoot_id)
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// The server never hands out a paused shoot's jobs, and a pause that
    /// lands mid-job reaches this worker as a cancellation on the heartbeat.
    fn is_shoot_paused(&self, _shoot_id: i64) -> bool {
        false
    }

    /// The server drops a cancelled shoot's remaining jobs itself; this
    /// worker only has to stop what it is running, which the flag does.
    fn cancel_shoot_jobs(&self, shoot_id: i64) {
        // A cancelled shoot's flag would otherwise pin every later job of
        // that shoot on this machine; the server will not hand any out
        // until the shoot is resumed, at which point the flag must be clear.
        self.cancel_flags.lock().remove(&shoot_id);
    }

    fn settings(&self) -> AppSettings {
        AppSettings::compose(self.library.read().clone(), self.machine.read().clone())
    }

    fn settings_version(&self) -> u64 {
        self.settings_version.load(Ordering::Relaxed)
    }

    fn record_blockage(&self, shoot_id: i64, _kind: &str, reason: &str) {
        self.blockages.lock().insert(shoot_id, reason.to_string());
    }

    fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    fn media_for(&self, job: &Job) -> Result<Media, WorkError> {
        self.claimed
            .lock()
            .get(&job.id)
            .map(|work| work.media.clone())
            .ok_or_else(|| WorkError::Failed("the job was not claimed by this worker".into()))
    }

    /// The share first, when the shoot has one and the path resolves here;
    /// a download otherwise. The download is kept until the job settles.
    fn source_path(&self, job: &Job, media: &Media) -> Result<PathBuf, WorkError> {
        let client_path = self.claimed.lock().get(&job.id).and_then(|work| work.client_path.clone());
        if let Some(path) = client_path.map(PathBuf::from).filter(|path| path.is_file()) {
            return Ok(path);
        }
        let downloaded = self.download_original(job, media)?;
        if let Some(work) = self.claimed.lock().get_mut(&job.id) {
            work.download = Some(downloaded.clone());
        }
        Ok(downloaded)
    }

    /// Review frames go up first, one request each, then the JSON. The server
    /// applies and settles the job in the result call; the answer is kept
    /// for the `complete` the worker loop makes next.
    fn deliver(&self, job: &Job, media: &Media, output: AnalysisOutput) -> Result<(), WorkError> {
        let token = Self::token_of(job).to_string();
        for frame in &output.review_frames {
            self.send(
                self.http
                    .post(self.url(&format!("{}?t={}", Self::job_path(job.id, "artifact"), frame.timestamp)))
                    .header(LEASE_TOKEN_HEADER, &token)
                    .header("content-type", "image/jpeg")
                    .body(frame.jpeg.clone()),
            )
            .map_err(WorkError::from)?;
        }
        let request = ResultRequest { token, output };
        let answer: SettleResponse = self
            .post_json(&Self::job_path(job.id, "result"), &request)
            .map_err(WorkError::from)?;
        self.note_ok();
        tracing::info!(job = job.id, file = %media.filename, settled = answer.settled, "analysis delivered");
        self.settled.lock().insert(
            job.id,
            if answer.settled { Settled::Done } else { Settled::LeaseLost },
        );
        Ok(())
    }
}
