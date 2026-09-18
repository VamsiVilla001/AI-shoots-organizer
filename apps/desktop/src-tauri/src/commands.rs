//! The Tauri IPC surface: one command per row of the core's registry.
//!
//! Nothing here has logic of its own. Each generated command builds a [`Ctx`]
//! for the calling window, runs the core's function on a worker thread — no
//! command may block the main thread, and the core is synchronous by design —
//! and hands the result back over IPC. The HTTP server does the same from the
//! same table, which is what keeps the two front doors from drifting.
//!
//! Commands that only make sense with a window — opening a path in Explorer,
//! restarting the app, native folder pickers — live in the modules beside
//! this one and are registered by hand.

use std::sync::Arc;

use serde::Serialize;
use skwad_app_core::api::{ApiError, Ctx, IdentityStore, Session, SessionUser, StoredIdentity};
use tauri::{AppHandle, State};
use zeroize::Zeroize;

use crate::state::AppState;

/// Errors cross the IPC boundary as a plain message; the UI shows it verbatim,
/// so the text has to be something a person can act on.
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
}

impl<E: std::fmt::Display> From<E> for CommandError {
    fn from(e: E) -> Self {
        CommandError { message: e.to_string() }
    }
}

impl CommandError {
    /// `ApiError` deliberately has no `Display`, so it cannot use the blanket
    /// conversion above; the message is all the bridge forwards.
    pub fn from_api(e: ApiError) -> Self {
        CommandError { message: e.message }
    }
}

pub type Result<T> = std::result::Result<T, CommandError>;

// --- identity -----------------------------------------------------------------

/// The desktop keeps the signed-in account's device identity in the operating
/// system credential store — Windows Credential Manager, the macOS keychain —
/// under one entry per machine.
const CREDENTIAL_SERVICE: &str = "com.skwad.mediaorganiser";
const CREDENTIAL_ACCOUNT: &str = "authenticated-device";

pub struct KeyringIdentityStore;

impl KeyringIdentityStore {
    fn entry() -> skwad_app_core::api::Result<keyring::Entry> {
        keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_ACCOUNT).map_err(|e| ApiError::internal(e.to_string()))
    }
}

impl IdentityStore for KeyringIdentityStore {
    fn load(&self) -> skwad_app_core::api::Result<StoredIdentity> {
        let mut value = Self::entry()?
            .get_password()
            .map_err(|_| ApiError::unauthorized("sign in first"))?;
        let result = serde_json::from_str(&value).map_err(|e| ApiError::internal(e.to_string()));
        value.zeroize();
        result
    }

    fn save(&self, identity: &StoredIdentity) -> skwad_app_core::api::Result<()> {
        let mut value = serde_json::to_string(identity).map_err(|e| ApiError::internal(e.to_string()))?;
        let result = Self::entry()?
            .set_password(&value)
            .map_err(|e| ApiError::internal(e.to_string()));
        value.zeroize();
        result
    }

    fn clear(&self) -> skwad_app_core::api::Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    }
}

/// The context for a command issued from this window.
///
/// The session is whoever the credential store says signed in; role checks
/// happen in the core against the credential file, as they always did.
pub fn ctx_for(app: &AppHandle, state: &Arc<AppState>) -> Ctx {
    headless_ctx_with_sink(Arc::clone(state), crate::events::sink(app))
}

/// A context with no window behind it — the Premiere bridge's, which runs on
/// its own thread and has nothing to emit to.
pub fn headless_ctx(state: Arc<AppState>) -> Ctx {
    headless_ctx_with_sink(state, skwad_app_core::null_sink())
}

fn headless_ctx_with_sink(state: Arc<AppState>, sink: Arc<dyn skwad_app_core::ProgressSink>) -> Ctx {
    let identity: Arc<dyn IdentityStore> = Arc::new(KeyringIdentityStore);
    let user = identity.load().ok().map(|identity| SessionUser {
        account_id: identity.account_id,
        email: identity.email,
        display_name: identity.display_name,
    });
    Ctx {
        state,
        sink,
        session: Session { user, scope: None },
        identity,
    }
}

// --- generated commands -----------------------------------------------------

/// Expands one registry row into a Tauri command.
macro_rules! tauri_commands {
    ($( $name:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty = $path:path ; )*) => { $(
        #[tauri::command]
        pub async fn $name(app: AppHandle, state: State<'_, Arc<AppState>>, $($arg: $ty),*) -> Result<$ret> {
            let ctx = ctx_for(&app, state.inner());
            tauri::async_runtime::spawn_blocking(move || $path(&ctx $(, $arg)*))
                .await
                .map_err(|e| CommandError { message: format!("the command stopped unexpectedly: {e}") })?
                .map_err(CommandError::from_api)
        }
    )* };
}

#[allow(unused_imports)]
mod generated {
    use super::*;
    use skwad_app_core::api::catalogue::*;
    use skwad_app_core::api::commands::*;
    use skwad_app_core::api::machines::*;
    use skwad_app_core::api::roster::*;
    use skwad_app_core::api::storage::*;
    use skwad_app_core::models::ModelStatus;
    use skwad_app_core::settings::AppSettings;
    use skwad_catalogue::{CatalogueGroup, CatalogueMedia};
    use skwad_database::models::*;
    use skwad_database::repo::roster::RosterEntry;
    use skwad_export_engine::ExportOptions;

    skwad_app_core::command_registry!(tauri_commands);
}

pub use generated::*;
