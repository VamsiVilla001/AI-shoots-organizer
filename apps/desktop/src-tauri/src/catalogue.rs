//! Shared encrypted catalogue commands.
//!
//! Device secrets and cached sessions live in the operating-system credential
//! store. Decrypted catalogue databases live only in this process's memory.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use argon2::{
    password_hash::{PasswordHasher, SaltString},
    Argon2, PasswordHash, PasswordVerifier,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD as B64, Engine};
use serde::{Deserialize, Serialize};
use skwad_catalogue::{
    build_payload, build_portable_catalogue, calibrate_argon2id, catalogue_groups, catalogue_media, catalogue_summary,
    generate_device_keypair, inspect_header, open_package, prepare_package, resolve_beneath_root, CatalogueGroup,
    CatalogueManifest, CatalogueMedia, CatalogueSummary, DeviceKeyPair, OpenCredential, PackageLimits, PublishOptions,
    Recipient,
};
use skwad_database::rusqlite::params;
use tauri::{AppHandle, State};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    commands::{CommandError, Result},
    state::AppState,
};

const CREDENTIAL_SERVICE: &str = "com.skwad.mediaorganiser";
const CREDENTIAL_ACCOUNT: &str = "authenticated-device";
const DEFAULT_BACKEND: &str = "http://127.0.0.1:8787";
const LOCAL_AUTH_VERSION: u32 = 1;
const MAX_AUTH_FILE_BYTES: u64 = 1024 * 1024;
const PROFILE_KEY_PREFIX: &str = "local_user_profile:";

pub struct LoadedCatalogue {
    pub package_id: String,
    pub revision_id: String,
    pub summary: CatalogueSummary,
    catalogue: Zeroizing<Vec<u8>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub authenticated_once: bool,
    pub password_change_required: bool,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub device_key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub job_title: Option<String>,
    pub organisation: Option<String>,
    pub location: Option<String>,
    pub bio: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdate {
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub job_title: Option<String>,
    pub organisation: Option<String>,
    pub location: Option<String>,
    pub bio: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    pub package_id: String,
    pub revision_id: String,
    pub path: String,
    pub media_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedCatalogueInfo {
    pub package_id: String,
    pub revision_id: String,
    #[serde(flatten)]
    pub summary: CatalogueSummary,
    pub mapped_root: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredIdentity {
    account_id: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    workspace_id: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    device_id: String,
    device_key_id: String,
    device_private_key: String,
    device_public_key: String,
    trusted_signing_keys: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendKeys {
    signing_key_id: String,
    signing_public_key: String,
    wrapping_key_id: String,
    wrapping_public_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalAuthFile {
    version: u32,
    users: Vec<LocalCredential>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalCredential {
    #[serde(default)]
    id: Option<String>,
    email: String,
    display_name: String,
    password_hash: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    must_change_password: bool,
}

#[derive(Debug)]
struct LocalAccount {
    id: String,
    email: String,
    display_name: String,
}

#[tauri::command]
pub fn catalogue_session_status(state: State<'_, Arc<AppState>>) -> Result<SessionStatus> {
    let identity = load_identity().ok();
    let active_identity = identity.and_then(|identity| {
        let auth = load_local_auth(&state).ok()?;
        auth.users
            .iter()
            .any(|user| user.enabled && !user.must_change_password && user.email.eq_ignore_ascii_case(&identity.email))
            .then_some(identity)
    });
    match active_identity {
        Some(identity) => Ok(SessionStatus {
            authenticated_once: true,
            password_change_required: false,
            account_id: Some(identity.account_id),
            email: Some(identity.email),
            device_key_id: Some(identity.device_key_id),
        }),
        None => Ok(SessionStatus {
            authenticated_once: false,
            password_change_required: false,
            account_id: None,
            email: None,
            device_key_id: None,
        }),
    }
}

#[tauri::command]
pub async fn sign_in_skwad(state: State<'_, Arc<AppState>>, email: String, password: String) -> Result<SessionStatus> {
    let password = Zeroizing::new(password);
    let account = authenticate_local(&state, &email, &password)?;
    let auth = load_local_auth(&state)?;
    let requires_change = auth
        .users
        .iter()
        .any(|user| user.enabled && user.email.eq_ignore_ascii_case(&account.email) && user.must_change_password);
    if requires_change {
        return Ok(SessionStatus {
            authenticated_once: false,
            password_change_required: true,
            account_id: Some(account.id),
            email: Some(account.email),
            device_key_id: None,
        });
    }
    establish_local_identity(&state, account).await
}

#[tauri::command]
pub async fn change_initial_password(
    state: State<'_, Arc<AppState>>,
    email: String,
    current_password: String,
    new_password: String,
) -> Result<SessionStatus> {
    if new_password.chars().count() < 10 {
        return Err(command_error("the new password must contain at least 10 characters"));
    }
    if current_password == new_password {
        return Err(command_error(
            "choose a new password different from the temporary password",
        ));
    }
    let current_password = Zeroizing::new(current_password);
    let new_password = Zeroizing::new(new_password);
    let account = authenticate_local(&state, &email, &current_password)?;
    update_local_password(&state, &account.email, &new_password)?;
    establish_local_identity(&state, account).await
}

async fn establish_local_identity(state: &AppState, account: LocalAccount) -> Result<SessionStatus> {
    let account_id = account.id;
    let existing = load_identity()
        .ok()
        .filter(|identity| identity.account_id == account_id);
    let (device_id, device_key_id, device_private_key, device_public_key, mut trusted_signing_keys) = match existing {
        Some(identity) => (
            identity.device_id,
            identity.device_key_id,
            identity.device_private_key,
            identity.device_public_key,
            identity.trusted_signing_keys,
        ),
        None => {
            let device = generate_device_keypair(format!("device-{}", Uuid::new_v4()));
            (
                Uuid::new_v4().to_string(),
                device.key_id.clone(),
                B64.encode(device.private_key_bytes()),
                B64.encode(device.public_key_bytes()),
                HashMap::new(),
            )
        }
    };
    if let Ok(backend_keys) = fetch_backend_keys().await {
        trusted_signing_keys.insert(backend_keys.signing_key_id, backend_keys.signing_public_key);
    }
    ensure_local_profile(state, &account_id, &account.email, &account.display_name)?;
    save_identity(&StoredIdentity {
        account_id: account_id.clone(),
        email: account.email.clone(),
        display_name: account.display_name,
        workspace_id: format!("local-{account_id}"),
        access_token: String::new(),
        refresh_token: String::new(),
        device_id,
        device_key_id: device_key_id.clone(),
        device_private_key,
        device_public_key,
        trusted_signing_keys,
    })?;
    Ok(SessionStatus {
        authenticated_once: true,
        password_change_required: false,
        account_id: Some(account_id),
        email: Some(account.email),
        device_key_id: Some(device_key_id),
    })
}

#[tauri::command]
pub fn clear_authenticated_session() -> Result<()> {
    credential_entry()?.delete_credential().map_err(command_error)?;
    Ok(())
}

#[tauri::command]
pub fn sign_out_skwad(state: State<'_, Arc<AppState>>) -> Result<()> {
    state.loaded_catalogues.lock().clear();
    clear_authenticated_session()
}

#[tauri::command]
pub fn get_user_profile(state: State<'_, Arc<AppState>>) -> Result<UserProfile> {
    let identity = load_identity().map_err(|_| command_error("sign in to view your profile"))?;
    load_local_profile(&state, &identity)
}

#[tauri::command]
pub fn update_user_profile(state: State<'_, Arc<AppState>>, update: ProfileUpdate) -> Result<UserProfile> {
    let identity = load_identity().map_err(|_| command_error("sign in to update your profile"))?;

    let display_name = update.display_name.trim().to_owned();
    if display_name.is_empty() || display_name.chars().count() > 80 {
        return Err(command_error("display name must contain between 1 and 80 characters"));
    }
    let avatar_url = clean_optional(update.avatar_url, 2048, "avatar URL")?;
    if let Some(url) = &avatar_url {
        let parsed = reqwest::Url::parse(url).map_err(|_| command_error("avatar URL is invalid"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(command_error("avatar URL must use HTTPS or HTTP"));
        }
    }
    let previous = load_local_profile(&state, &identity)?;
    let profile = UserProfile {
        user_id: identity.account_id.clone(),
        email: identity.email.clone(),
        display_name,
        avatar_url,
        job_title: clean_optional(update.job_title, 120, "job title")?,
        organisation: clean_optional(update.organisation, 160, "organisation")?,
        location: clean_optional(update.location, 120, "location")?,
        bio: clean_optional(update.bio, 500, "bio")?,
        created_at: previous.created_at,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    save_local_profile(&state, &profile)?;
    Ok(profile)
}

#[tauri::command]
pub async fn publish_skwad(
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
    destination: String,
    passphrase: String,
) -> Result<PublishResult> {
    let mut identity =
        load_identity().map_err(|_| command_error("sign in once before publishing a SKWAD catalogue"))?;
    let keys = fetch_backend_keys().await?;
    identity
        .trusted_signing_keys
        .insert(keys.signing_key_id.clone(), keys.signing_public_key.clone());
    save_identity(&identity)?;
    let device = identity.device_key()?;
    let backend_public = B64.decode(&keys.wrapping_public_key).map_err(command_error)?;
    let package_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let db = state.db.clone();
    let passphrase = Zeroizing::new(passphrase);
    let own_recipient = Recipient {
        key_id: device.key_id.clone(),
        public_key: device.public_key_bytes().to_vec(),
    };
    let backend_recipient = Recipient {
        key_id: keys.wrapping_key_id,
        public_key: backend_public,
    };
    let signing_key_id = keys.signing_key_id;
    let unsigned =
        tauri::async_runtime::spawn_blocking(move || -> Result<(Vec<u8>, u64, i64, String, String, String)> {
            let conn = db.conn().map_err(command_error)?;
            let revision_number: i64 = conn
                .query_row(
                    "SELECT coalesce(max(revision_number),0)+1 FROM catalogue_revisions WHERE shoot_id=?1",
                    [shoot_id],
                    |row| row.get(0),
                )
                .map_err(command_error)?;
            let portable = build_portable_catalogue(&conn, shoot_id, revision_number as u64).map_err(command_error)?;
            let shoot_name: String = conn
                .query_row("SELECT name FROM shoots WHERE id=?1", [shoot_id], |row| row.get(0))
                .map_err(command_error)?;
            let library_id = portable.library_id.clone();
            let stable_shoot_id = portable.shoot_id.clone();
            let payload = Zeroizing::new(
                build_payload(
                    CatalogueManifest {
                        schema_version: 1,
                        library_id: portable.library_id,
                        shoot_id: portable.shoot_id,
                        published_revision: revision_number as u64,
                        created_at: chrono::Utc::now().to_rfc3339(),
                        catalogue_blake3: String::new(),
                        media_count: portable.media_count,
                    },
                    &portable.bytes,
                )
                .map_err(command_error)?,
            );
            let bytes = prepare_package(
                &payload,
                PublishOptions {
                    package_id,
                    revision_id,
                    recipients: &[own_recipient, backend_recipient],
                    passphrase: Some(&passphrase),
                    passphrase_key_id: "offline-owner",
                    argon2: calibrate_argon2id(500),
                },
                &signing_key_id,
            )
            .map_err(command_error)?;
            Ok((
                bytes,
                portable.media_count,
                revision_number,
                library_id,
                stable_shoot_id,
                shoot_name,
            ))
        })
        .await
        .map_err(command_error)??;

    let signed = reqwest::Client::new()
        .post(format!("{}/v1/packages/sign", backend_url()))
        .bearer_auth(backend_auth_token()?)
        .header("x-skwad-workspace-id", &identity.workspace_id)
        .header(reqwest::header::CONTENT_TYPE, "application/vnd.skwad.catalogue")
        .body(unsigned.0)
        .send()
        .await
        .map_err(command_error)?;
    if !signed.status().is_success() {
        return Err(command_error(format!(
            "backend refused publication: {}",
            signed.text().await.unwrap_or_default()
        )));
    }
    let package = signed.bytes().await.map_err(command_error)?;
    let destination = package_destination(&destination);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(command_error)?;
    }
    std::fs::write(&destination, &package).map_err(command_error)?;
    let conn = state.db.conn().map_err(command_error)?;
    conn.execute("INSERT INTO catalogue_revisions(revision_id,package_id,shoot_id,revision_number,state,manifest_hash,created_at,published_at) VALUES(?1,?2,?3,?4,'published',?5,?6,?6)", params![revision_id.to_string(), package_id.to_string(), shoot_id, unsigned.2, blake3::hash(&package).to_hex().to_string(), chrono::Utc::now().to_rfc3339()]).map_err(command_error)?;
    Ok(PublishResult {
        package_id: package_id.to_string(),
        revision_id: revision_id.to_string(),
        path: destination.to_string_lossy().into_owned(),
        media_count: unsigned.1,
    })
}

#[tauri::command]
pub async fn load_skwad(
    state: State<'_, Arc<AppState>>,
    path: String,
    passphrase: Option<String>,
) -> Result<LoadedCatalogueInfo> {
    let identity = load_identity()
        .map_err(|_| command_error("this computer must sign in to a SKWAD account once before loading catalogues"))?;
    let metadata = std::fs::metadata(&path).map_err(command_error)?;
    if metadata.len() > 513 * 1024 * 1024 {
        return Err(command_error("SKWAD package exceeds the 512 MiB safety limit"));
    }
    let package = std::fs::read(&path).map_err(command_error)?;
    let header = inspect_header(&package, PackageLimits::default()).map_err(command_error)?;
    if !identity
        .trusted_signing_keys
        .contains_key(&header.authenticated.signing_key_id)
    {
        return Err(command_error(
            "the package uses an unknown signing key; sign in again to trust an authorised key rotation",
        ));
    }
    let verifying_key = identity.verifying_key(&header.authenticated.signing_key_id)?;
    let device = identity.device_key()?;
    let decoded = match open_package(
        &package,
        OpenCredential::Device(&device),
        &verifying_key,
        PackageLimits::default(),
    ) {
        Ok(decoded) => decoded,
        Err(skwad_catalogue::CatalogueError::NoMatchingRecipient) if passphrase.is_some() => {
            let phrase = Zeroizing::new(passphrase.unwrap());
            open_package(
                &package,
                OpenCredential::Passphrase(&phrase),
                &verifying_key,
                PackageLimits::default(),
            )
            .map_err(command_error)?
        }
        Err(error) => return Err(command_error(error)),
    };
    let summary = catalogue_summary(&decoded.catalogue).map_err(command_error)?;
    state.db.conn().map_err(command_error)?.execute("INSERT INTO imported_catalogues(package_id,revision_id,library_id,shoot_id,catalogue_hash,imported_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(package_id,revision_id) DO UPDATE SET imported_at=excluded.imported_at", params![header.authenticated.package_id.to_string(), header.authenticated.revision_id.to_string(), summary.library_id, summary.shoot_id, blake3::hash(&package).to_hex().to_string(), chrono::Utc::now().to_rfc3339()]).map_err(command_error)?;
    let key = catalogue_key(
        &header.authenticated.package_id.to_string(),
        &header.authenticated.revision_id.to_string(),
    );
    state.loaded_catalogues.lock().insert(
        key,
        LoadedCatalogue {
            package_id: header.authenticated.package_id.to_string(),
            revision_id: header.authenticated.revision_id.to_string(),
            summary: summary.clone(),
            catalogue: decoded.catalogue,
        },
    );
    loaded_info(
        &state,
        &header.authenticated.package_id.to_string(),
        &header.authenticated.revision_id.to_string(),
        summary,
    )
}

#[tauri::command]
pub fn approve_catalogue_library(
    state: State<'_, Arc<AppState>>,
    package_id: String,
    revision_id: String,
    root: String,
) -> Result<LoadedCatalogueInfo> {
    let path = PathBuf::from(&root);
    if !path.is_dir() {
        return Err(command_error("choose an existing NAS library folder"));
    }
    let key = catalogue_key(&package_id, &revision_id);
    let loaded = state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&key)
        .ok_or_else(|| command_error("load the catalogue before mapping its library"))?;
    let now = chrono::Utc::now().to_rfc3339();
    state.db.conn().map_err(command_error)?.execute("INSERT INTO library_mappings(library_id,label,local_root,approved_at,updated_at) VALUES(?1,?2,?3,?4,?4) ON CONFLICT(library_id) DO UPDATE SET local_root=excluded.local_root,updated_at=excluded.updated_at", params![catalogue.summary.library_id, catalogue.summary.shoot_name, path.to_string_lossy(), now]).map_err(command_error)?;
    loaded_info(&state, &package_id, &revision_id, catalogue.summary.clone())
}

#[tauri::command]
pub fn list_loaded_catalogues(state: State<'_, Arc<AppState>>) -> Result<Vec<LoadedCatalogueInfo>> {
    state
        .loaded_catalogues
        .lock()
        .values()
        .map(|catalogue| {
            loaded_info(
                &state,
                &catalogue.package_id,
                &catalogue.revision_id,
                catalogue.summary.clone(),
            )
        })
        .collect()
}

#[tauri::command]
pub fn list_catalogue_groups(
    state: State<'_, Arc<AppState>>,
    package_id: String,
    revision_id: String,
) -> Result<Vec<CatalogueGroup>> {
    let loaded = state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&catalogue_key(&package_id, &revision_id))
        .ok_or_else(|| command_error("catalogue is not loaded"))?;
    catalogue_groups(&catalogue.catalogue).map_err(command_error)
}

#[tauri::command]
pub fn list_catalogue_media(
    state: State<'_, Arc<AppState>>,
    package_id: String,
    revision_id: String,
    group_id: Option<i64>,
) -> Result<Vec<CatalogueMedia>> {
    let loaded = state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&catalogue_key(&package_id, &revision_id))
        .ok_or_else(|| command_error("catalogue is not loaded"))?;
    catalogue_media(&catalogue.catalogue, group_id).map_err(command_error)
}

#[tauri::command]
pub fn open_catalogue_media(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    package_id: String,
    revision_id: String,
    media_id: i64,
) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let loaded = state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&catalogue_key(&package_id, &revision_id))
        .ok_or_else(|| command_error("catalogue is not loaded"))?;
    let media = catalogue_media(&catalogue.catalogue, None)
        .map_err(command_error)?
        .into_iter()
        .find(|item| item.id == media_id)
        .ok_or_else(|| command_error("media reference is missing"))?;
    let root: String = state
        .db
        .conn()
        .map_err(command_error)?
        .query_row(
            "SELECT local_root FROM library_mappings WHERE library_id=?1",
            [&catalogue.summary.library_id],
            |row| row.get(0),
        )
        .map_err(|_| command_error("map this catalogue to an approved NAS root first"))?;
    let target = resolve_beneath_root(Path::new(&root), &media.relative_path).map_err(command_error)?;
    let canonical_root = std::fs::canonicalize(&root).map_err(command_error)?;
    let canonical_target = std::fs::canonicalize(&target).map_err(command_error)?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(command_error("media reference resolves outside the approved NAS root"));
    }
    app.opener()
        .open_path(canonical_target.to_string_lossy().into_owned(), None::<&str>)
        .map_err(command_error)
}

fn loaded_info(
    state: &AppState,
    package_id: &str,
    revision_id: &str,
    summary: CatalogueSummary,
) -> Result<LoadedCatalogueInfo> {
    let mapped_root = state
        .db
        .conn()
        .map_err(command_error)?
        .query_row(
            "SELECT local_root FROM library_mappings WHERE library_id=?1",
            [&summary.library_id],
            |row| row.get::<_, String>(0),
        )
        .ok();
    Ok(LoadedCatalogueInfo {
        package_id: package_id.into(),
        revision_id: revision_id.into(),
        summary,
        mapped_root,
    })
}

fn auth_file_path(state: &AppState) -> PathBuf {
    std::env::var_os("SKWAD_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| state.paths.root.join("auth").join("credentials.json"))
}

fn load_local_auth(state: &AppState) -> Result<LocalAuthFile> {
    let path = auth_file_path(state);
    let metadata = std::fs::metadata(&path).map_err(|_| {
        command_error(format!(
            "local credential file not found at {}; set SKWAD_AUTH_FILE or create the default file",
            path.display()
        ))
    })?;
    if metadata.len() > MAX_AUTH_FILE_BYTES {
        return Err(command_error("local credential file exceeds the 1 MiB safety limit"));
    }
    let bytes = std::fs::read(&path).map_err(command_error)?;
    let auth: LocalAuthFile = serde_json::from_slice(&bytes)
        .map_err(|error| command_error(format!("local credential file is invalid: {error}")))?;
    if auth.version != LOCAL_AUTH_VERSION {
        return Err(command_error(format!(
            "unsupported local credential file version {}",
            auth.version
        )));
    }
    Ok(auth)
}

fn authenticate_local(state: &AppState, email: &str, password: &str) -> Result<LocalAccount> {
    let email = email.trim();
    let auth = load_local_auth(state)?;
    let user = auth
        .users
        .iter()
        .find(|user| user.enabled && user.email.eq_ignore_ascii_case(email))
        .ok_or_else(|| command_error("email or password is incorrect"))?;
    let parsed = PasswordHash::new(&user.password_hash)
        .map_err(|_| command_error("the credential file contains an invalid password hash"))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| command_error("email or password is incorrect"))?;
    let canonical_email = user.email.trim().to_lowercase();
    let id = user
        .id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("local-{}", &blake3::hash(canonical_email.as_bytes()).to_hex()[..32]));
    let display_name = user.display_name.trim();
    if canonical_email.is_empty() || display_name.is_empty() || display_name.chars().count() > 80 {
        return Err(command_error("the credential file contains an invalid user record"));
    }
    Ok(LocalAccount {
        id,
        email: canonical_email,
        display_name: display_name.to_owned(),
    })
}

fn update_local_password(state: &AppState, email: &str, new_password: &str) -> Result<()> {
    let path = auth_file_path(state);
    let mut auth = load_local_auth(state)?;
    let user = auth
        .users
        .iter_mut()
        .find(|user| user.enabled && user.email.eq_ignore_ascii_case(email))
        .ok_or_else(|| command_error("the local account no longer exists"))?;
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(command_error)?;
    user.password_hash = Argon2::default()
        .hash_password(new_password.as_bytes(), &salt)
        .map_err(command_error)?
        .to_string();
    user.must_change_password = false;
    let encoded = serde_json::to_vec_pretty(&auth).map_err(command_error)?;
    let parent = path
        .parent()
        .ok_or_else(|| command_error("credential file path has no parent directory"))?;
    std::fs::create_dir_all(parent).map_err(command_error)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, &encoded).map_err(command_error)?;
    std::fs::copy(&temporary, &path).map_err(command_error)?;
    let _ = std::fs::remove_file(temporary);
    Ok(())
}

fn profile_key(account_id: &str) -> String {
    format!("{PROFILE_KEY_PREFIX}{account_id}")
}

fn ensure_local_profile(state: &AppState, account_id: &str, email: &str, display_name: &str) -> Result<()> {
    let conn = state.db.conn().map_err(command_error)?;
    let key = profile_key(account_id);
    if skwad_database::repo::settings::get_raw(&conn, &key)
        .map_err(command_error)?
        .is_some()
    {
        return Ok(());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let profile = UserProfile {
        user_id: account_id.to_owned(),
        email: email.to_owned(),
        display_name: display_name.to_owned(),
        avatar_url: None,
        job_title: None,
        organisation: None,
        location: None,
        bio: None,
        created_at: now.clone(),
        updated_at: now,
    };
    skwad_database::repo::settings::set(&conn, &key, &profile).map_err(command_error)
}

fn load_local_profile(state: &AppState, identity: &StoredIdentity) -> Result<UserProfile> {
    ensure_local_profile(
        state,
        &identity.account_id,
        &identity.email,
        if identity.display_name.is_empty() {
            identity.email.split('@').next().unwrap_or("SKWAD user")
        } else {
            &identity.display_name
        },
    )?;
    let conn = state.db.conn().map_err(command_error)?;
    skwad_database::repo::settings::get_raw(&conn, &profile_key(&identity.account_id))
        .map_err(command_error)?
        .ok_or_else(|| command_error("the local profile was not found"))
        .and_then(|value| serde_json::from_str(&value).map_err(command_error))
}

fn save_local_profile(state: &AppState, profile: &UserProfile) -> Result<()> {
    let conn = state.db.conn().map_err(command_error)?;
    skwad_database::repo::settings::set(&conn, &profile_key(&profile.user_id), profile).map_err(command_error)
}

fn default_enabled() -> bool {
    true
}

fn clean_optional(value: Option<String>, max: usize, label: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(command_error(format!("{label} must not exceed {max} characters")));
    }
    Ok(Some(value.to_owned()))
}

async fn fetch_backend_keys() -> Result<BackendKeys> {
    reqwest::Client::new()
        .get(format!("{}/v1/crypto/public-keys", backend_url()))
        .send()
        .await
        .map_err(command_error)?
        .error_for_status()
        .map_err(command_error)?
        .json()
        .await
        .map_err(command_error)
}

fn backend_auth_token() -> Result<String> {
    std::env::var("SKWAD_BACKEND_AUTH_TOKEN")
        .ok()
        .filter(|token| token.len() >= 24)
        .ok_or_else(|| command_error("SKWAD_BACKEND_AUTH_TOKEN is not configured"))
}

fn backend_url() -> String {
    std::env::var("SKWAD_BACKEND_URL")
        .unwrap_or_else(|_| DEFAULT_BACKEND.into())
        .trim_end_matches('/')
        .to_owned()
}
fn package_destination(value: &str) -> PathBuf {
    let mut path = PathBuf::from(value);
    if path
        .extension()
        .and_then(|v| v.to_str())
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("skwad"))
    {
        path.set_extension("skwad");
    }
    path
}
fn catalogue_key(package_id: &str, revision_id: &str) -> String {
    format!("{package_id}:{revision_id}")
}
fn credential_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_ACCOUNT).map_err(command_error)
}
fn load_identity() -> Result<StoredIdentity> {
    let mut value = credential_entry()?.get_password().map_err(command_error)?;
    let result = serde_json::from_str(&value).map_err(command_error);
    value.zeroize();
    result
}
fn save_identity(identity: &StoredIdentity) -> Result<()> {
    let mut value = serde_json::to_string(identity).map_err(command_error)?;
    let result = credential_entry()?.set_password(&value).map_err(command_error);
    value.zeroize();
    result
}
fn command_error(error: impl std::fmt::Display) -> CommandError {
    CommandError {
        message: error.to_string(),
    }
}

impl StoredIdentity {
    fn device_key(&self) -> Result<DeviceKeyPair> {
        DeviceKeyPair::from_bytes(
            &self.device_key_id,
            B64.decode(&self.device_private_key).map_err(command_error)?,
            B64.decode(&self.device_public_key).map_err(command_error)?,
        )
        .map_err(command_error)
    }
    fn verifying_key(&self, id: &str) -> Result<[u8; 32]> {
        B64.decode(
            self.trusted_signing_keys
                .get(id)
                .ok_or_else(|| command_error("the package signing key is not trusted"))?,
        )
        .map_err(command_error)?
        .try_into()
        .map_err(|_| command_error("invalid trusted signing key"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use argon2::{
        password_hash::{PasswordHasher, SaltString},
        Argon2,
    };
    use skwad_database::Database;
    use uuid::Uuid;

    use super::{
        authenticate_local, clean_optional, load_local_auth, update_local_password, LocalAuthFile, LocalCredential,
    };
    use crate::{paths::AppPaths, settings::AppSettings, state::AppState};

    #[test]
    fn optional_profile_fields_are_trimmed() {
        assert_eq!(
            clean_optional(Some("  Esports producer  ".into()), 120, "job title").unwrap(),
            Some("Esports producer".into())
        );
    }

    #[test]
    fn blank_profile_fields_become_null() {
        assert_eq!(clean_optional(Some("   ".into()), 120, "job title").unwrap(), None);
        assert_eq!(clean_optional(None, 120, "job title").unwrap(), None);
    }

    #[test]
    fn oversized_profile_fields_are_rejected() {
        assert!(clean_optional(Some("12345".into()), 4, "field").is_err());
    }

    #[test]
    fn local_credentials_require_a_hash_and_forceable_first_change() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::create(temp.path()).unwrap();
        let auth_dir = paths.root.join("auth");
        std::fs::create_dir_all(&auth_dir).unwrap();
        let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).unwrap();
        let hash = Argon2::default()
            .hash_password(b"temporary-password", &salt)
            .unwrap()
            .to_string();
        let document = LocalAuthFile {
            version: 1,
            users: vec![LocalCredential {
                id: Some("local-test-user".into()),
                email: "person@example.com".into(),
                display_name: "Person".into(),
                password_hash: hash,
                enabled: true,
                must_change_password: true,
            }],
        };
        std::fs::write(
            auth_dir.join("credentials.json"),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            Database::open_in_memory().unwrap(),
            paths,
            AppSettings::default(),
            "skwadmedia://".into(),
        ));

        assert!(authenticate_local(&state, "PERSON@example.com", "temporary-password").is_ok());
        assert!(authenticate_local(&state, "person@example.com", "wrong-password").is_err());
        update_local_password(&state, "person@example.com", "a-new-private-password").unwrap();
        assert!(authenticate_local(&state, "person@example.com", "temporary-password").is_err());
        assert!(authenticate_local(&state, "person@example.com", "a-new-private-password").is_ok());
        assert!(!load_local_auth(&state).unwrap().users[0].must_change_password);
    }

    #[test]
    fn plaintext_password_fields_are_rejected() {
        let json = r#"{"version":1,"users":[{"email":"person@example.com","displayName":"Person","password":"unsafe","passwordHash":"hash","enabled":true}]}"#;
        assert!(serde_json::from_str::<LocalAuthFile>(json).is_err());
    }
}
