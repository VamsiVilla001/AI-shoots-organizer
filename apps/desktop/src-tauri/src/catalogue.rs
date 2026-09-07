//! Shared encrypted catalogue commands.
//!
//! Device secrets and cached sessions live in the operating-system credential
//! store. Decrypted catalogue databases live only in this process's memory.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
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
    pub account_id: Option<String>,
    pub device_key_id: Option<String>,
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
    workspace_id: String,
    access_token: String,
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

#[derive(Debug, Deserialize)]
struct AuthResponse {
    access_token: String,
    refresh_token: String,
    user: AuthUser,
}
#[derive(Debug, Deserialize)]
struct AuthUser {
    id: String,
}
#[derive(Debug, Deserialize)]
struct WorkspaceRow {
    id: String,
}

#[tauri::command]
pub fn catalogue_session_status() -> Result<SessionStatus> {
    match load_identity() {
        Ok(identity) => Ok(SessionStatus {
            authenticated_once: true,
            account_id: Some(identity.account_id),
            device_key_id: Some(identity.device_key_id),
        }),
        Err(_) => Ok(SessionStatus {
            authenticated_once: false,
            account_id: None,
            device_key_id: None,
        }),
    }
}

#[tauri::command]
pub async fn sign_in_skwad(email: String, password: String) -> Result<SessionStatus> {
    let url = std::env::var("SKWAD_SUPABASE_URL").map_err(|_| command_error("SKWAD_SUPABASE_URL is not configured"))?;
    let anon = std::env::var("SKWAD_SUPABASE_ANON_KEY")
        .map_err(|_| command_error("SKWAD_SUPABASE_ANON_KEY is not configured"))?;
    let password = Zeroizing::new(password);
    let response = reqwest::Client::new()
        .post(format!(
            "{}/auth/v1/token?grant_type=password",
            url.trim_end_matches('/')
        ))
        .header("apikey", &anon)
        .json(&serde_json::json!({"email": email, "password": password.as_str()}))
        .send()
        .await
        .map_err(command_error)?;
    if !response.status().is_success() {
        return Err(command_error("SKWAD account sign-in failed"));
    }
    let auth: AuthResponse = response.json().await.map_err(command_error)?;
    let account_id = auth.user.id;
    let workspace_id = personal_workspace(&url, &anon, &auth.access_token, &account_id, &email).await?;
    let backend_keys = fetch_backend_keys().await?;
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
    trusted_signing_keys.insert(backend_keys.signing_key_id, backend_keys.signing_public_key);
    let registration = reqwest::Client::new().post(format!("{}/rest/v1/devices?on_conflict=id", url.trim_end_matches('/')))
        .header("apikey", &anon).bearer_auth(&auth.access_token).header("Prefer", "resolution=merge-duplicates")
        .json(&serde_json::json!({"id": device_id, "user_id": account_id, "opaque_key_id": device_key_id, "hpke_public_key": device_public_key, "label": std::env::var("COMPUTERNAME").unwrap_or_else(|_| "SKWAD desktop".into())}))
        .send().await.map_err(command_error)?;
    if !registration.status().is_success() {
        return Err(command_error(
            "signed in, but the device public key could not be registered",
        ));
    }
    save_identity(&StoredIdentity {
        account_id: account_id.clone(),
        workspace_id,
        access_token: auth.access_token,
        refresh_token: auth.refresh_token,
        device_id,
        device_key_id: device_key_id.clone(),
        device_private_key,
        device_public_key,
        trusted_signing_keys,
    })?;
    Ok(SessionStatus {
        authenticated_once: true,
        account_id: Some(account_id),
        device_key_id: Some(device_key_id),
    })
}

#[tauri::command]
pub fn clear_authenticated_session() -> Result<()> {
    credential_entry()?.delete_credential().map_err(command_error)?;
    Ok(())
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
    refresh_session(&mut identity).await?;
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

    begin_cloud_revision(
        &identity,
        package_id,
        revision_id,
        unsigned.2,
        &unsigned.3,
        &unsigned.4,
        &unsigned.5,
    )
    .await?;

    let signed = reqwest::Client::new()
        .post(format!("{}/v1/packages/sign", backend_url()))
        .bearer_auth(&identity.access_token)
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
    finish_cloud_revision(&identity, package_id, revision_id, &package).await?;
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

async fn refresh_session(identity: &mut StoredIdentity) -> Result<()> {
    let url = std::env::var("SKWAD_SUPABASE_URL").map_err(|_| command_error("SKWAD_SUPABASE_URL is not configured"))?;
    let anon = std::env::var("SKWAD_SUPABASE_ANON_KEY")
        .map_err(|_| command_error("SKWAD_SUPABASE_ANON_KEY is not configured"))?;
    let response = reqwest::Client::new()
        .post(format!(
            "{}/auth/v1/token?grant_type=refresh_token",
            url.trim_end_matches('/')
        ))
        .header("apikey", anon)
        .json(&serde_json::json!({"refresh_token": identity.refresh_token}))
        .send()
        .await
        .map_err(command_error)?;
    if !response.status().is_success() {
        return Err(command_error(
            "your SKWAD session expired; sign in again before publishing",
        ));
    }
    let auth: AuthResponse = response.json().await.map_err(command_error)?;
    if auth.user.id != identity.account_id {
        return Err(command_error("refreshed account does not match this device"));
    }
    identity.access_token = auth.access_token;
    identity.refresh_token = auth.refresh_token;
    save_identity(identity)
}

async fn personal_workspace(url: &str, anon: &str, token: &str, account_id: &str, email: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let rows: Vec<WorkspaceRow> = client
        .get(format!(
            "{}/rest/v1/workspaces?select=id&kind=eq.personal&owner_id=eq.{}&limit=1",
            url.trim_end_matches('/'),
            account_id
        ))
        .header("apikey", anon)
        .bearer_auth(token)
        .send()
        .await
        .map_err(command_error)?
        .error_for_status()
        .map_err(command_error)?
        .json()
        .await
        .map_err(command_error)?;
    if let Some(row) = rows.into_iter().next() {
        return Ok(row.id);
    }
    let created: Vec<WorkspaceRow> = client.post(format!("{}/rest/v1/workspaces", url.trim_end_matches('/')))
        .header("apikey", anon).bearer_auth(token).header("Prefer", "return=representation")
        .json(&serde_json::json!({"kind":"personal","name":format!("{}'s workspace", email.split('@').next().unwrap_or("SKWAD")),"owner_id":account_id}))
        .send().await.map_err(command_error)?.error_for_status().map_err(command_error)?.json().await.map_err(command_error)?;
    created
        .into_iter()
        .next()
        .map(|row| row.id)
        .ok_or_else(|| command_error("Supabase did not create a personal workspace"))
}

async fn begin_cloud_revision(
    identity: &StoredIdentity,
    package_id: Uuid,
    revision_id: Uuid,
    revision_number: i64,
    library_id: &str,
    shoot_id: &str,
    shoot_name: &str,
) -> Result<()> {
    let (url, anon) = supabase_config()?;
    let client = reqwest::Client::new();
    for (endpoint, body) in [
        (
            "libraries",
            serde_json::json!({"id":library_id,"workspace_id":identity.workspace_id,"label":shoot_name}),
        ),
        (
            "shoots",
            serde_json::json!({"id":shoot_id,"workspace_id":identity.workspace_id,"library_id":library_id,"name":shoot_name,"cloud_revision":revision_number}),
        ),
    ] {
        client
            .post(format!("{}/rest/v1/{}?on_conflict=id", url, endpoint))
            .header("apikey", &anon)
            .bearer_auth(&identity.access_token)
            .header("Prefer", "resolution=merge-duplicates")
            .json(&body)
            .send()
            .await
            .map_err(command_error)?
            .error_for_status()
            .map_err(command_error)?;
    }
    client.post(format!("{url}/rest/v1/catalogue_revisions")).header("apikey", &anon).bearer_auth(&identity.access_token)
        .json(&serde_json::json!({"id":revision_id,"package_id":package_id,"workspace_id":identity.workspace_id,"shoot_id":shoot_id,"revision_number":revision_number,"state":"draft","created_by":identity.account_id}))
        .send().await.map_err(command_error)?.error_for_status().map_err(command_error)?;
    Ok(())
}

async fn finish_cloud_revision(
    identity: &StoredIdentity,
    package_id: Uuid,
    revision_id: Uuid,
    package: &[u8],
) -> Result<()> {
    let (url, anon) = supabase_config()?;
    let object_key = format!("{}/{}/{}.skwad", identity.workspace_id, package_id, revision_id);
    let client = reqwest::Client::new();
    client
        .post(format!("{url}/storage/v1/object/skwad-packages/{object_key}"))
        .header("apikey", &anon)
        .bearer_auth(&identity.access_token)
        .header(reqwest::header::CONTENT_TYPE, "application/vnd.skwad.catalogue")
        .body(package.to_vec())
        .send()
        .await
        .map_err(command_error)?
        .error_for_status()
        .map_err(command_error)?;
    client.patch(format!("{url}/rest/v1/catalogue_revisions?id=eq.{revision_id}")).header("apikey", &anon).bearer_auth(&identity.access_token)
        .json(&serde_json::json!({"state":"published","object_key":object_key,"ciphertext_blake3":blake3::hash(package).to_hex().to_string(),"published_at":chrono::Utc::now().to_rfc3339()}))
        .send().await.map_err(command_error)?.error_for_status().map_err(command_error)?;
    Ok(())
}

fn supabase_config() -> Result<(String, String)> {
    Ok((
        std::env::var("SKWAD_SUPABASE_URL")
            .map_err(|_| command_error("SKWAD_SUPABASE_URL is not configured"))?
            .trim_end_matches('/')
            .to_owned(),
        std::env::var("SKWAD_SUPABASE_ANON_KEY")
            .map_err(|_| command_error("SKWAD_SUPABASE_ANON_KEY is not configured"))?,
    ))
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
