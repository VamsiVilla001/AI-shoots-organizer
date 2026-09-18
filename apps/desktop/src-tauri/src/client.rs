//! Client mode: this installation talks to a SKWAD server instead of a library.
//!
//! A client holds one thing — the server's address — and never a database
//! credential. The webview does everything over HTTP against that server
//! (see `transport.ts`); this module is what remains on the desktop side:
//!
//! * `client.json` in the app data folder, which is what says "client mode"
//!   at startup. `SKWAD_SERVER_URL` overrides it.
//! * Worker mode. Once an administrator enrols this machine, its token is
//!   kept here and, when the person switches the worker on, the app runs the
//!   same analysis slots the server does, fed by `/api/work/*` through a
//!   [`RemoteJobSource`]. Nothing else about the app changes: the same
//!   engine, the same models, the same cancellation flags.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use skwad_app_core::{AppPaths, MachineSettings, ProgressSink, RemoteConfig, RemoteJobSource, RemoteStatus, WorkerPool};
use tauri::{AppHandle, Manager};

use crate::commands::{CommandError, Result};

pub const CONFIG_FILE: &str = "client.json";
const CONFIG_VERSION: u32 = 1;
const SERVER_ENV: &str = "SKWAD_SERVER_URL";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientConfig {
    pub version: u32,
    /// `https://studio-pc:8420`. `None` means this installation owns (or
    /// joins) a library directly, the way it always has.
    pub server_url: Option<String>,
    /// The machine token an administrator's enrolment handed out. Kept
    /// here rather than in the credential store because it belongs to the
    /// installation, not to whoever is signed in.
    pub machine_token: Option<String>,
    /// The name the machine was enrolled under, for the settings screen.
    pub machine_name: Option<String>,
    /// Whether the worker starts with the app.
    pub worker_enabled: bool,
}

pub fn config_path(app_data: &Path) -> PathBuf {
    app_data.join(CONFIG_FILE)
}

pub fn load(app_data: &Path) -> ClientConfig {
    std::fs::read(config_path(app_data))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ClientConfig>(&bytes).ok())
        .filter(|config| config.version <= CONFIG_VERSION)
        .unwrap_or_default()
}

pub fn save(app_data: &Path, config: &ClientConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(app_data)?;
    let mut config = config.clone();
    config.version = CONFIG_VERSION;
    std::fs::write(config_path(app_data), serde_json::to_vec_pretty(&config)?)
}

/// The server this launch should talk to, if any. The environment wins, as
/// it does for the library root.
pub fn resolve_server(app_data: &Path) -> Option<String> {
    std::env::var(SERVER_ENV)
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .or_else(|| load(app_data).server_url)
        .map(|url| url.trim_end_matches('/').to_string())
}

fn validate_url(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/');
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(CommandError::from("the server address must start with http:// or https://"));
    }
    if trimmed.len() <= "https://".len() {
        return Err(CommandError::from("enter the server address, for example https://studio-pc:8420"));
    }
    Ok(trimmed.to_string())
}

// --- the running mode -------------------------------------------------------------

struct RunningWorker {
    source: Arc<RemoteJobSource>,
    pool: Option<WorkerPool>,
    keeper: Option<std::thread::JoinHandle<()>>,
}

/// What the app manages in client mode.
pub struct ClientMode {
    app_data: PathBuf,
    /// This machine's own folders: models, downloads, logs.
    pub paths: AppPaths,
    pub machine_id: String,
    config: Mutex<ClientConfig>,
    worker: Mutex<Option<RunningWorker>>,
    /// Set while a start is in progress on its own thread.
    starting: AtomicBool,
    last_error: Mutex<Option<String>>,
}

/// What the worker panel shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    pub server_url: Option<String>,
    pub machine_id: String,
    pub machine_name: Option<String>,
    pub enrolled: bool,
    pub enabled: bool,
    pub starting: bool,
    pub last_error: Option<String>,
    pub remote: Option<RemoteStatus>,
    pub machine_settings: MachineSettings,
}

impl ClientMode {
    pub fn new(app_data: PathBuf, paths: AppPaths, machine_id: String, config: ClientConfig) -> Self {
        Self {
            app_data,
            paths,
            machine_id,
            config: Mutex::new(config),
            worker: Mutex::new(None),
            starting: AtomicBool::new(false),
            last_error: Mutex::new(None),
        }
    }

    pub fn config(&self) -> ClientConfig {
        self.config.lock().clone()
    }

    fn machine_settings_file(&self) -> PathBuf {
        skwad_app_core::settings::machine_settings_path(&self.app_data)
    }

    pub fn machine_settings(&self) -> MachineSettings {
        MachineSettings::load(&self.machine_settings_file()).unwrap_or_default()
    }

    fn update_config(&self, edit: impl FnOnce(&mut ClientConfig)) -> Result<ClientConfig> {
        let mut config = self.config.lock();
        edit(&mut config);
        save(&self.app_data, &config).map_err(|e| CommandError::from(format!("could not write {CONFIG_FILE}: {e}")))?;
        Ok(config.clone())
    }

    pub fn status(&self) -> WorkerStatus {
        let config = self.config();
        WorkerStatus {
            server_url: config.server_url,
            machine_id: self.machine_id.clone(),
            machine_name: config.machine_name,
            enrolled: config.machine_token.is_some(),
            enabled: config.worker_enabled,
            starting: self.starting.load(Ordering::Relaxed),
            last_error: self.last_error.lock().clone(),
            remote: self.worker.lock().as_ref().map(|running| running.source.status()),
            machine_settings: self.machine_settings(),
        }
    }

    /// Connects, syncs models and starts the analysis slots — on its own
    /// thread, because the model sync can be hundreds of megabytes and the
    /// person who flipped the switch wants the window back.
    pub fn start_worker(self: &Arc<Self>, sink: Arc<dyn ProgressSink>) -> Result<()> {
        let config = self.config();
        let Some(server_url) = config.server_url.clone() else {
            return Err(CommandError::from("this installation is not connected to a server"));
        };
        let Some(token) = config.machine_token.clone() else {
            return Err(CommandError::from("this machine is not enrolled as a worker yet; an administrator enrols it from Settings"));
        };
        if self.worker.lock().is_some() || self.starting.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        *self.last_error.lock() = None;

        let mode = Arc::clone(self);
        std::thread::Builder::new()
            .name("skwad-worker-start".into())
            .spawn(move || {
                let outcome = (|| -> std::result::Result<RunningWorker, String> {
                    let source = RemoteJobSource::connect(
                        RemoteConfig {
                            base_url: server_url,
                            machine_token: token,
                            machine_id: mode.machine_id.clone(),
                        },
                        mode.paths.clone(),
                        mode.machine_settings(),
                    )?;
                    source.sync_models()?;
                    let keeper = source.start_keeper();
                    let pool = WorkerPool::start_remote(sink, source.clone(), mode.paths.clone());
                    Ok(RunningWorker {
                        source,
                        pool: Some(pool),
                        keeper: Some(keeper),
                    })
                })();
                match outcome {
                    Ok(running) => {
                        tracing::info!("worker mode started");
                        *mode.worker.lock() = Some(running);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "worker mode could not start");
                        *mode.last_error.lock() = Some(error);
                    }
                }
                mode.starting.store(false, Ordering::Relaxed);
            })
            .map_err(|e| CommandError::from(format!("could not start the worker: {e}")))?;
        Ok(())
    }

    /// Stops claiming and lets the jobs in flight finish or lapse.
    pub fn stop_worker(&self) {
        let Some(mut running) = self.worker.lock().take() else { return };
        running.source.shutdown();
        let pool = running.pool.take();
        let keeper = running.keeper.take();
        // Joining waits for the current file to finish; not on the caller's
        // thread, which is the UI's.
        std::thread::Builder::new()
            .name("skwad-worker-stop".into())
            .spawn(move || {
                if let Some(pool) = pool {
                    pool.join();
                }
                if let Some(keeper) = keeper {
                    let _ = keeper.join();
                }
                tracing::info!("worker mode stopped");
            })
            .ok();
    }

    pub fn is_running(&self) -> bool {
        self.worker.lock().is_some()
    }
}

// ---------------------------------------------------------------- commands
//
// None of these take `State<AppState>`: in client mode there is no library
// and therefore no such state.

fn app_data(app: &AppHandle) -> Result<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| CommandError::from(format!("could not resolve the application data directory: {e}")))
}

fn mode(app: &AppHandle) -> Result<Arc<ClientMode>> {
    app.try_state::<Arc<ClientMode>>()
        .map(|state| Arc::clone(state.inner()))
        .ok_or_else(|| CommandError::from("this installation is not running in client mode"))
}

/// The worker panel's view. Answers in library mode too, with no server,
/// so the settings screen can tell which mode it is in.
#[tauri::command]
pub fn client_status(app: AppHandle) -> Result<WorkerStatus> {
    match app.try_state::<Arc<ClientMode>>() {
        Some(mode) => Ok(mode.status()),
        None => {
            let data = app_data(&app)?;
            Ok(WorkerStatus {
                server_url: None,
                machine_id: skwad_app_core::machine::load_or_create(&data),
                machine_name: None,
                enrolled: false,
                enabled: false,
                starting: false,
                last_error: None,
                remote: None,
                machine_settings: MachineSettings::load(&skwad_app_core::settings::machine_settings_path(&data))
                    .unwrap_or_default(),
            })
        }
    }
}

/// Points this installation at a server (or, with `None`, back at a library
/// of its own). Takes effect at the next launch.
#[tauri::command]
pub fn set_server_url(app: AppHandle, url: Option<String>) -> Result<bool> {
    let data = app_data(&app)?;
    let url = url.map(|u| validate_url(&u)).transpose()?;
    let mut config = load(&data);
    let changed = config.server_url != url;
    if changed {
        // A different server is a different enrolment.
        config.machine_token = None;
        config.machine_name = None;
        config.worker_enabled = false;
    }
    config.server_url = url;
    save(&data, &config).map_err(|e| CommandError::from(format!("could not write {CONFIG_FILE}: {e}")))?;
    if let Some(mode) = app.try_state::<Arc<ClientMode>>() {
        *mode.config.lock() = config;
    }
    Ok(changed)
}

/// Restarts so the new mode is used, the way the database change does.
#[tauri::command]
pub fn restart_for_client_change(app: AppHandle) {
    if let Some(state) = app.try_state::<Arc<crate::state::AppState>>() {
        state.begin_shutdown();
    }
    if let Some(mode) = app.try_state::<Arc<ClientMode>>() {
        mode.stop_worker();
    }
    app.restart();
}

/// Keeps the token an enrolment handed out. The enrolment itself is the
/// `enrol_machine` command, run against the server by an administrator.
#[tauri::command]
pub fn store_machine_enrolment(app: AppHandle, token: String, name: String) -> Result<WorkerStatus> {
    let mode = mode(&app)?;
    if token.trim().is_empty() {
        return Err(CommandError::from("the enrolment did not include a token"));
    }
    mode.update_config(|config| {
        config.machine_token = Some(token.trim().to_string());
        config.machine_name = Some(name.trim().to_string());
    })?;
    // A running worker holds the old token; restart it on the new one.
    if mode.is_running() {
        mode.stop_worker();
        mode.start_worker(crate::events::sink(&app))?;
    }
    Ok(mode.status())
}

/// Forgets the enrolment on this side. The administrator revokes it on the
/// server with `revoke_machine`.
#[tauri::command]
pub fn forget_machine_enrolment(app: AppHandle) -> Result<WorkerStatus> {
    let mode = mode(&app)?;
    mode.stop_worker();
    mode.update_config(|config| {
        config.machine_token = None;
        config.machine_name = None;
        config.worker_enabled = false;
    })?;
    Ok(mode.status())
}

/// Switches the worker on or off, and remembers the choice for next launch.
#[tauri::command]
pub fn set_worker_enabled(app: AppHandle, enabled: bool) -> Result<WorkerStatus> {
    let mode = mode(&app)?;
    mode.update_config(|config| config.worker_enabled = enabled)?;
    if enabled {
        mode.start_worker(crate::events::sink(&app))?;
    } else {
        mode.stop_worker();
    }
    Ok(mode.status())
}

/// This machine's half of the settings — its worker count, accelerator,
/// FFmpeg — which in client mode the server's settings screen must not
/// touch. Applied to a running worker at its next job.
#[tauri::command]
pub fn update_worker_settings(app: AppHandle, settings: MachineSettings) -> Result<WorkerStatus> {
    let mode = mode(&app)?;
    let settings = skwad_app_core::AppSettings::compose(Default::default(), settings)
        .sanitised()
        .machine();
    settings
        .save(&mode.machine_settings_file())
        .map_err(|e| CommandError::from(format!("could not save the machine settings: {e}")))?;
    if let Some(running) = mode.worker.lock().as_ref() {
        running.source.set_machine_settings(settings);
    }
    Ok(mode.status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_round_trips_and_the_environment_wins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_server(dir.path()).is_none() || std::env::var_os(SERVER_ENV).is_some());
        save(
            dir.path(),
            &ClientConfig {
                server_url: Some("https://studio-pc:8420/".into()),
                machine_token: Some("t".into()),
                worker_enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.version, CONFIG_VERSION);
        assert_eq!(loaded.machine_token.as_deref(), Some("t"));
        assert!(loaded.worker_enabled);
        if std::env::var_os(SERVER_ENV).is_none() {
            assert_eq!(resolve_server(dir.path()).as_deref(), Some("https://studio-pc:8420"));
        }
    }

    #[test]
    fn server_urls_are_checked_before_they_are_kept() {
        assert!(validate_url("studio-pc:8420").is_err());
        assert!(validate_url("https://").is_err());
        assert_eq!(validate_url(" https://studio-pc:8420/ ").unwrap(), "https://studio-pc:8420");
    }
}
