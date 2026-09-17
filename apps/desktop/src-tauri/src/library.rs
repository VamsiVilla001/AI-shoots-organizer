//! Where the shared library lives.
//!
//! SKWAD is a local application that a whole team can share: one workstation
//! (or a NAS) holds the library folder, everybody else points at the same
//! UNC path, and the database, caches, face embeddings, user profiles and the
//! credential file are then common to the team.
//!
//! The pointer itself cannot live in the database — the database is the thing
//! it locates — so it is a small JSON file in each machine's own application
//! data directory:
//!
//! ```text
//! %APPDATA%\com.skwad.mediaorganiser\library.json
//! ```
//!
//! `SKWAD_LIBRARY_ROOT` overrides it, which is how a deployment script or a
//! test points an installation somewhere else without touching the file.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::{
    commands::{CommandError, Result},
    state::AppState,
};

const CONFIG_FILE: &str = "library.json";
const CONFIG_VERSION: u32 = 1;
const ROOT_ENV: &str = "SKWAD_LIBRARY_ROOT";

/// This machine's own application data directory, remembered at startup so the
/// commands can find `library.json` after `AppPaths` has moved to the share.
static APP_DATA: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LibraryConfig {
    pub version: u32,
    /// The shared library folder. `None` keeps everything in app data.
    pub root: Option<String>,
    /// Optional per-machine folder for thumbnails, proxies and face crops.
    /// Keeping those local spares the network the bulky, rebuildable files.
    pub cache_root: Option<String>,
    /// Overrides the automatic network detection when a share is reached
    /// through a mapped drive letter rather than a UNC path.
    pub network_share: Option<bool>,
}

/// The locations an installation is actually running on.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub root: PathBuf,
    pub cache_root: Option<PathBuf>,
    pub network_share: bool,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// `SKWAD_LIBRARY_ROOT` was set.
    Environment,
    /// An administrator pointed this machine at a shared folder.
    Configured,
    /// Nothing is configured: the library is this machine's app data.
    AppData,
}

/// What the Settings screen shows and edits.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryLocation {
    /// The folder this session opened. Changing the location needs a restart,
    /// so this and `configured_root` differ until the app is restarted.
    pub active_root: String,
    pub active_cache_root: String,
    pub configured_root: Option<String>,
    pub configured_cache_root: Option<String>,
    pub source: Source,
    pub network_share: bool,
    pub database_file: String,
    pub app_data_root: String,
    pub restart_required: bool,
    /// Set when another installation has already put a library there, so the
    /// UI can say "joining the team library" rather than "creating one".
    pub existing_library: bool,
}

pub fn remember_app_data(dir: impl Into<PathBuf>) {
    let _ = APP_DATA.set(dir.into());
}

fn app_data() -> Result<PathBuf> {
    APP_DATA
        .get()
        .cloned()
        .ok_or_else(|| command_error("the application data directory is not known yet"))
}

pub fn config_path(app_data: &Path) -> PathBuf {
    app_data.join(CONFIG_FILE)
}

pub fn load(app_data: &Path) -> LibraryConfig {
    std::fs::read(config_path(app_data))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LibraryConfig>(&bytes).ok())
        .filter(|config| config.version <= CONFIG_VERSION)
        .unwrap_or_default()
}

pub fn save(app_data: &Path, config: &LibraryConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(app_data)?;
    let mut config = config.clone();
    config.version = CONFIG_VERSION;
    std::fs::write(config_path(app_data), serde_json::to_vec_pretty(&config)?)
}

/// Works out which folders this launch should use. A configured share that has
/// gone offline falls back to app data rather than refusing to start, so a
/// laptop off the network still opens.
pub fn resolve(app_data: &Path) -> Resolved {
    let config = load(app_data);
    let from_env = std::env::var_os(ROOT_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let configured = config.root.as_deref().map(PathBuf::from);

    let (root, source) = match (from_env, configured) {
        (Some(path), _) => (path, Source::Environment),
        (None, Some(path)) if reachable(&path) => (path, Source::Configured),
        _ => (app_data.to_path_buf(), Source::AppData),
    };
    let network_share = config.network_share.unwrap_or_else(|| is_network_path(&root));
    let cache_root = config
        .cache_root
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path != &root);
    Resolved {
        root,
        cache_root,
        network_share,
        source,
    }
}

/// A UNC path (`\\\\nas\\skwad`) is the shape a shared library normally takes.
/// A mapped drive letter cannot be told apart from a local disk without asking
/// Windows, so the Settings screen carries a manual override for that case.
pub fn is_network_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("\\\\") || text.starts_with("//")
}

fn reachable(path: &Path) -> bool {
    path.is_dir() || std::fs::create_dir_all(path).is_ok()
}

/// Confirms SKWAD can actually create and delete files there. A share mounted
/// read-only fails here rather than halfway through the first import.
pub fn probe_writable(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|error| command_error(format!("{} cannot be created: {error}", dir.display())))?;
    let probe = dir.join(format!(".skwad-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"skwad")
        .map_err(|error| command_error(format!("{} is not writable: {error}", dir.display())))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

// --- commands -------------------------------------------------------------

#[tauri::command]
pub fn get_library_location(state: State<'_, Arc<AppState>>) -> Result<LibraryLocation> {
    let app_data = app_data()?;
    Ok(describe(&state, &app_data, &load(&app_data)))
}

/// Points this installation at a shared library folder. Nothing is copied:
/// after the restart SKWAD opens the library that is already there, or creates
/// an empty one if the folder is new.
#[tauri::command]
pub fn set_library_location(
    state: State<'_, Arc<AppState>>,
    root: Option<String>,
    cache_root: Option<String>,
    network_share: Option<bool>,
) -> Result<LibraryLocation> {
    let app_data = app_data()?;
    let root = clean_path(root);
    let cache_root = clean_path(cache_root);

    if let Some(root) = &root {
        let path = PathBuf::from(root);
        probe_writable(&path)?;
        if let Some(cache) = &cache_root {
            if Path::new(cache) == path {
                return Err(command_error("the cache folder must differ from the library folder"));
            }
        }
    } else if cache_root.is_some() {
        return Err(command_error("choose a library folder before setting a cache folder"));
    }
    if let Some(cache) = &cache_root {
        probe_writable(Path::new(cache))?;
    }

    let config = LibraryConfig {
        version: CONFIG_VERSION,
        root,
        cache_root,
        network_share,
    };
    save(&app_data, &config).map_err(|error| command_error(format!("could not save the library location: {error}")))?;
    tracing::info!(root = ?config.root, cache = ?config.cache_root, "library location changed");
    Ok(describe(&state, &app_data, &config))
}

/// Restarts so the new location takes effect. The workers are told to stop
/// first, exactly as they are when the window is closed.
#[tauri::command]
pub fn restart_for_library_change(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<()> {
    state.begin_shutdown();
    app.restart();
}

fn describe(state: &AppState, app_data: &Path, config: &LibraryConfig) -> LibraryLocation {
    let active_root = state.paths.root.clone();
    let configured_root = config.root.clone();
    let active_cache_root = state.paths.thumbnails.parent().unwrap_or(&active_root).to_path_buf();
    let restart_required = configured_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| app_data.to_path_buf())
        != active_root
        || config
            .cache_root
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| active_root.clone())
            != active_cache_root;
    let resolved = resolve(app_data);
    LibraryLocation {
        active_root: active_root.display().to_string(),
        active_cache_root: active_cache_root.display().to_string(),
        configured_root,
        configured_cache_root: config.cache_root.clone(),
        source: resolved.source,
        network_share: resolved.network_share,
        database_file: state.paths.database_file().display().to_string(),
        app_data_root: app_data.display().to_string(),
        restart_required,
        existing_library: config
            .root
            .as_deref()
            .map(|root| Path::new(root).join("database").join("media.db").is_file())
            .unwrap_or(false),
    }
}

fn clean_path(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().trim_end_matches(['/', '\\']).to_owned())
        .filter(|value| !value.is_empty())
}

fn command_error(error: impl std::fmt::Display) -> CommandError {
    CommandError {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_configured_keeps_the_library_in_app_data() {
        let temp = tempfile::tempdir().unwrap();
        let resolved = resolve(temp.path());
        assert_eq!(resolved.root, temp.path());
        assert_eq!(resolved.source, Source::AppData);
        assert!(resolved.cache_root.is_none());
        assert!(!resolved.network_share);
    }

    #[test]
    fn a_configured_share_is_used_and_survives_a_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let share = temp.path().join("team-library");
        std::fs::create_dir_all(&share).unwrap();
        let cache = temp.path().join("local-cache");
        save(
            temp.path(),
            &LibraryConfig {
                version: 1,
                root: Some(share.display().to_string()),
                cache_root: Some(cache.display().to_string()),
                network_share: Some(true),
            },
        )
        .unwrap();

        let resolved = resolve(temp.path());
        assert_eq!(resolved.root, share);
        assert_eq!(resolved.cache_root.as_deref(), Some(cache.as_path()));
        assert_eq!(resolved.source, Source::Configured);
        assert!(resolved.network_share, "the manual override wins over path shape");
    }

    #[test]
    fn unc_paths_are_recognised_as_shares() {
        assert!(is_network_path(Path::new(r"\\nas\skwad")));
        assert!(!is_network_path(Path::new(r"D:\skwad")));
    }

    #[test]
    fn a_read_only_or_missing_location_is_reported_before_it_is_saved() {
        let temp = tempfile::tempdir().unwrap();
        assert!(probe_writable(&temp.path().join("new-folder")).is_ok());
    }

    #[test]
    fn trailing_separators_are_trimmed() {
        assert_eq!(clean_path(Some(r"\\nas\skwad\".into())), Some(r"\\nas\skwad".into()));
        assert_eq!(clean_path(Some("   ".into())), None);
    }
}
