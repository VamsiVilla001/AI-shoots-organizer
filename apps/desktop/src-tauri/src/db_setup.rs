//! Configuring the library database from inside the app.
//!
//! Before this existed, pointing a machine at a library meant hand-writing two
//! files — `database.json` for the address and `pgpass.conf` for the password —
//! and the only feedback was the app failing to start. That is an unreasonable
//! thing to ask of someone installing a photo application, and it went wrong on
//! the first two machines it was tried on.
//!
//! So: when the database cannot be opened the app still starts, and the window
//! shows a setup screen instead of the library. The screen tests a connection
//! before saving it, so the answer arrives while the person is still looking at
//! the form rather than at a dialog on the next launch.
//!
//! ## Where the password lives
//!
//! The operating system's credential store — Windows Credential Manager, macOS
//! Keychain — via the same `keyring` the app already uses for device secrets.
//! Not `database.json`, because the library folder can be a network share and a
//! password in a shared folder is a password everyone has; and not a file the
//! person has to create, because that was the original problem.
//!
//! `pgpass.conf` is still read (see `PgConfig::resolve`) so machines set up the
//! old way keep working, but nothing needs it any more.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use skwad_database::{Database, PgConfig};
use tauri::{AppHandle, Manager, State};

use crate::commands::{CommandError, Result};
use crate::state::AppState;

/// Service name in the OS credential store. Matches the app identifier, and is
/// distinct from the catalogue's device-secret entry by its account key.
const CREDENTIAL_SERVICE: &str = "com.skwad.mediaorganiser";

/// What startup decided. The UI asks for this before anything else, because
/// when the database is unavailable there is no [`AppState`] and every other
/// command would fail.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum StartupStatus {
    /// The library opened; the app proper can render.
    Ready,
    /// The database could not be opened. Carries what to show on the setup
    /// screen: which server was tried, and why it did not work.
    NeedsDatabase {
        settings: DatabaseSettings,
        title: String,
        detail: String,
    },
    /// This installation is a client of a server: the webview should talk
    /// HTTP to `server_url` and there is no library here.
    Client {
        server_url: String,
        machine_id: String,
        machine_name: Option<String>,
        worker_enabled: bool,
    },
}

/// The connection, without the password. Mirrors `PgConfig`'s serialised shape
/// so the UI and `database.json` agree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSettings {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Whether a password for this exact server is already in the credential
    /// store, so the form can show "leave blank to keep the saved password"
    /// rather than making someone retype it to change a port.
    #[serde(default)]
    pub has_saved_password: bool,
}

impl DatabaseSettings {
    pub fn from_config(config: &PgConfig) -> Self {
        let mut settings = Self {
            host: config.host.clone(),
            port: config.port,
            database: config.database.clone(),
            user: config.user.clone(),
            has_saved_password: false,
        };
        settings.has_saved_password = load_password(&settings).is_some();
        settings
    }

    fn to_config(&self, password: Option<String>) -> PgConfig {
        PgConfig {
            host: self.host.trim().to_string(),
            port: self.port,
            database: self.database.trim().to_string(),
            user: self.user.trim().to_string(),
            password,
            ..PgConfig::default()
        }
    }

    /// Identifies the credential. Keyed by the whole target rather than a
    /// single "database password" entry so a machine can hold credentials for
    /// more than one library — a studio server and a local scratch database —
    /// without them overwriting each other.
    fn credential_key(&self) -> String {
        format!("database:{}@{}:{}/{}", self.user, self.host, self.port, self.database)
    }

    fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            return Err(CommandError::from("enter the server address"));
        }
        if self.database.trim().is_empty() {
            return Err(CommandError::from("enter the database name"));
        }
        if self.user.trim().is_empty() {
            return Err(CommandError::from("enter the user name"));
        }
        if self.port == 0 {
            return Err(CommandError::from("enter a port (PostgreSQL's default is 5432)"));
        }
        Ok(())
    }
}

fn entry(settings: &DatabaseSettings) -> Option<keyring::Entry> {
    keyring::Entry::new(CREDENTIAL_SERVICE, &settings.credential_key()).ok()
}

/// The saved password for this server, if there is one.
pub fn load_password(settings: &DatabaseSettings) -> Option<String> {
    entry(settings)?.get_password().ok()
}

fn save_password(settings: &DatabaseSettings, password: &str) -> Result<()> {
    entry(settings)
        .ok_or_else(|| CommandError::from("this machine has no usable credential store"))?
        .set_password(password)
        .map_err(|e| CommandError::from(format!("could not save the password: {e}")))
}

/// Fills in a password from the credential store when nothing else supplied one.
///
/// Called during startup, after `PgConfig::resolve` has applied
/// `SKWAD_DATABASE_URL`, `SKWAD_DATABASE_PASSWORD` and `pgpass.conf` — so an
/// explicit override still wins, and this only fills the gap that used to force
/// someone to write a password file by hand.
pub fn apply_saved_password(config: &mut PgConfig) {
    if config.password.is_some() {
        return;
    }
    let settings = DatabaseSettings {
        host: config.host.clone(),
        port: config.port,
        database: config.database.clone(),
        user: config.user.clone(),
        has_saved_password: false,
    };
    if let Some(password) = load_password(&settings) {
        tracing::info!("using the database password from the credential store");
        config.password = Some(password);
    }
}

// ---------------------------------------------------------------- commands
//
// None of these take `State<AppState>`: they have to work in the case where
// there is no database and therefore no state to take.

/// Whether the app can show the library, or needs setting up first.
#[tauri::command]
pub fn startup_status(status: State<'_, StartupStatus>) -> StartupStatus {
    status.inner().clone()
}

/// The connection this machine is configured for, for the settings form.
#[tauri::command]
pub fn database_settings(app: AppHandle) -> Result<DatabaseSettings> {
    // Read from disk rather than from the running config so the form shows what
    // is stored, which is what a person is about to edit.
    let root = library_root(&app)?;
    Ok(DatabaseSettings::from_config(&PgConfig::resolve(&root)))
}

/// Opens a connection and closes it, reporting what happened in words.
///
/// Deliberately separate from saving. Getting told the address is wrong while
/// the form is still on screen is the whole point; finding out at the next
/// launch is what this feature exists to stop.
#[tauri::command]
pub async fn test_database_connection(settings: DatabaseSettings, password: Option<String>) -> Result<String> {
    settings.validate()?;
    let password = resolve_password(&settings, password);

    // `Database::connect` blocks and runs migrations, so it cannot run on the
    // UI's async executor thread.
    let probe = settings.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let config = probe.to_config(password);
        match Database::connect(config) {
            Ok(db) => {
                let describe = db.describe();
                drop(db);
                Ok(format!("Connected to {describe}."))
            }
            Err(error) => Err(CommandError::from(explain(&error, &probe))),
        }
    })
    .await
    .map_err(|e| CommandError::from(format!("the connection test did not finish: {e}")))?
}

/// Saves the connection, after proving it works.
///
/// Refusing to save a connection that does not open is the point: a saved but
/// broken setting is indistinguishable, at the next launch, from never having
/// configured anything.
#[tauri::command]
pub async fn save_database_connection(
    app: AppHandle,
    settings: DatabaseSettings,
    password: Option<String>,
) -> Result<String> {
    settings.validate()?;
    let password = resolve_password(&settings, password);

    let probe = settings.clone();
    let probe_password = password.clone();
    let message =
        tauri::async_runtime::spawn_blocking(move || match Database::connect(probe.to_config(probe_password)) {
            Ok(db) => {
                let describe = db.describe();
                drop(db);
                Ok(describe)
            }
            Err(error) => Err(CommandError::from(explain(&error, &probe))),
        })
        .await
        .map_err(|e| CommandError::from(format!("the connection test did not finish: {e}")))??;

    let root = library_root(&app)?;
    settings
        .to_config(None)
        .save(&root)
        .map_err(|e| CommandError::from(format!("could not write database.json: {e}")))?;
    if let Some(password) = password {
        save_password(&settings, &password)?;
    }

    tracing::info!(server = %message, "saved a new library database connection");
    Ok(format!("Saved. SKWAD will reopen against {message}."))
}

/// Restarts so the new connection is used. The whole startup sequence then runs
/// normally, rather than this having to rebuild half of it in place.
#[tauri::command]
pub fn restart_for_database_change(app: AppHandle) {
    // `try_state` rather than a `State` argument: the common case for this
    // command is the app having come up *without* a database, so there is no
    // `AppState` to ask for and requiring one would make the command
    // uncallable exactly when it is needed.
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.begin_shutdown();
    }
    app.restart();
}

/// A blank password means "keep the one already saved", so changing a port does
/// not require retyping a credential the person may not have to hand.
fn resolve_password(settings: &DatabaseSettings, supplied: Option<String>) -> Option<String> {
    match supplied.map(|p| p.trim().to_string()) {
        Some(password) if !password.is_empty() => Some(password),
        _ => load_password(settings),
    }
}

fn library_root(app: &AppHandle) -> Result<std::path::PathBuf> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| CommandError::from(format!("could not resolve the application data directory: {e}")))?;
    Ok(crate::library::resolve(&data_dir).root)
}

/// Turns a connection failure into a sentence about this form's fields.
///
/// Shorter and more pointed than the startup dialog's version: the person is
/// looking at the settings that failed, so it only has to say which of them is
/// wrong.
fn explain(error: &skwad_database::DbError, settings: &DatabaseSettings) -> String {
    let where_it_looked = format!("{}:{}", settings.host, settings.port);
    if error.is_missing_credential() {
        format!("{where_it_looked} answered, but wants a password. Enter one above.")
    } else if error.is_rejected() {
        format!(
            "{where_it_looked} answered and refused the connection.\n\
             Check the password, that the user \"{}\" exists, and that the database \"{}\" exists on that server.",
            settings.user, settings.database
        )
    } else {
        format!(
            "Nothing answered at {where_it_looked}.\n\
             Check the address, that the server is running, and that its firewall allows this machine."
        )
    }
}
