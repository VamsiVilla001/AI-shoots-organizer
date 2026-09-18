//! Shared encrypted catalogue commands.
//!
//! Device secrets and cached sessions live in the operating-system credential
//! store. Decrypted catalogue databases live only in this process's memory.

use skwad_database::Db;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
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
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::api::{ApiError as CommandError, Ctx, Result};
use crate::state::{AppState, LoadedCatalogue};

const DEFAULT_BACKEND: &str = "http://127.0.0.1:8787";
const LOCAL_AUTH_VERSION: u32 = 1;
const MAX_AUTH_FILE_BYTES: u64 = 1024 * 1024;
const PROFILE_KEY_PREFIX: &str = "local_user_profile:";
/// Testing build: SKWAD accepts the seeded password and never forces a change
/// on first sign-in. Flip to `true` to restore the temporary-password flow.
const ENFORCE_PASSWORD_CHANGE: bool = false;
/// Password every seeded account starts with while the team tests the build.
const SEED_PASSWORD: &str = "Tess@123";
/// The team roster written to a fresh credential file on first launch.
const SEED_USERS: &[(&str, &str, UserRole)] = &[
    ("aashish.gupta@tesseractesports.com", "Aashish Gupta", UserRole::Member),
    ("aniket.ajeet@tesseractesports.com", "Aniket Ajeet", UserRole::Member),
    (
        "biswadeep.chamling@tesseractesports.com",
        "Biswadeep Chamling",
        UserRole::Member,
    ),
    (
        "dhanush.murugesan@tesseractesports.com",
        "Dhanush Murugesan",
        UserRole::Member,
    ),
    (
        "dharmesh.joshi@tesseractesports.com",
        "Dharmesh Joshi",
        UserRole::Member,
    ),
    ("gopi.maddi@tesseractesports.com", "Gopi Maddi", UserRole::Member),
    (
        "kartik.chaudhary@tesseractesports.com",
        "Kartik Chaudhary",
        UserRole::Member,
    ),
    (
        "mahendra.paljangir@tesseractesports.com",
        "Mahendra Paljangir",
        UserRole::Member,
    ),
    (
        "muhsin.noorsha@tesseractesports.com",
        "Muhsin Noorsha",
        UserRole::Member,
    ),
    ("prakash.ks@tesseractesports.com", "Prakash KS", UserRole::Member),
    ("praveen.anne@tesseractesports.com", "Praveen Anne", UserRole::Member),
    ("rahul.kambogi@tesseractesports.com", "Rahul Kambogi", UserRole::Member),
    ("rajesh.sarkar@tesseractesports.com", "Rajesh Sarkar", UserRole::Member),
    ("ritupol.kro@tesseractesports.com", "Ritupol Kro", UserRole::Member),
    (
        "saicharan.guda@tesseractesports.com",
        "Saicharan Guda",
        UserRole::Member,
    ),
    ("saif.mohammed@tesseractesports.com", "Saif Mohammed", UserRole::Member),
    (
        "sayan.dasgupta@tesseractesports.com",
        "Sayan Dasgupta",
        UserRole::Member,
    ),
    (
        "sumanth.sudamsetti@tesseractesports.com",
        "Sumanth Sudamsetti",
        UserRole::Member,
    ),
    ("suraj.sinha@tesseractesports.com", "Suraj Sinha", UserRole::Member),
    ("tarson.tokbi@tesseractesports.com", "Tarson Tokbi", UserRole::Member),
    ("vamsi.villa@tesseractesports.com", "Vamsi Villa", UserRole::Member),
    ("yash.patle@tesseractesports.com", "Yash Patle", UserRole::Member),
    (
        "naresh.nallamothu@tesseractesports.com",
        "Naresh Nallamothu",
        UserRole::Admin,
    ),
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub authenticated_once: bool,
    pub password_change_required: bool,
    pub is_admin: bool,
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

/// A signed-in account's device identity: who they are, this device's
/// catalogue keypair, and the package signing keys it trusts. Kept by an
/// [`IdentityStore`](crate::api::IdentityStore) — the desktop's is the OS
/// credential store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredIdentity {
    pub account_id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    pub device_id: String,
    pub device_key_id: String,
    pub device_private_key: String,
    pub device_public_key: String,
    pub trusted_signing_keys: HashMap<String, String>,
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
    #[serde(default)]
    role: UserRole,
}

/// What a local account may do. Only admins reach the user-management panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    #[default]
    Member,
}

/// One row of the admin panel. Password hashes never leave the backend.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalUser {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub role: UserRole,
    pub enabled: bool,
    pub must_change_password: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLocalUser {
    pub email: String,
    pub display_name: String,
    #[serde(default)]
    pub role: UserRole,
    /// Left empty, the account starts on the shared testing password.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalUserUpdate {
    pub email: String,
    pub display_name: String,
    pub role: UserRole,
    pub enabled: bool,
}

#[derive(Debug)]
struct LocalAccount {
    id: String,
    email: String,
    display_name: String,
    role: UserRole,
}

pub fn current_project_identity(ctx: &Ctx) -> Result<(String, String, Option<String>)> {
    let identity = ctx.identity.load().map_err(|_| CommandError::unauthorized("sign in to access projects"))?;
    let organisation = load_local_profile(&ctx.state, &identity)
        .ok()
        .and_then(|profile| profile.organisation);
    Ok((identity.account_id, identity.email, organisation))
}

pub fn catalogue_session_status(ctx: &Ctx) -> Result<SessionStatus> {
    let active = ctx.identity.load().ok().and_then(|identity| {
        let auth = load_local_auth(&ctx.state).ok()?;
        let role = auth
            .users
            .iter()
            .find(|user| {
                user.enabled
                    && (!ENFORCE_PASSWORD_CHANGE || !user.must_change_password)
                    && user.email.eq_ignore_ascii_case(&identity.email)
            })
            .map(|user| user.role)?;
        Some((identity, role))
    });
    match active {
        Some((identity, role)) => Ok(SessionStatus {
            authenticated_once: true,
            password_change_required: false,
            is_admin: role == UserRole::Admin,
            account_id: Some(identity.account_id),
            email: Some(identity.email),
            device_key_id: Some(identity.device_key_id),
        }),
        None => Ok(SessionStatus {
            authenticated_once: false,
            password_change_required: false,
            is_admin: false,
            account_id: None,
            email: None,
            device_key_id: None,
        }),
    }
}

pub fn sign_in_skwad(ctx: &Ctx, email: String, password: String) -> Result<SessionStatus> {
    let password = Zeroizing::new(password);
    let account = authenticate_local(&ctx.state, &email, &password)?;
    let auth = load_local_auth(&ctx.state)?;
    let requires_change = ENFORCE_PASSWORD_CHANGE
        && auth
            .users
            .iter()
            .any(|user| user.enabled && user.email.eq_ignore_ascii_case(&account.email) && user.must_change_password);
    if requires_change {
        return Ok(SessionStatus {
            authenticated_once: false,
            password_change_required: true,
            is_admin: false,
            account_id: Some(account.id),
            email: Some(account.email),
            device_key_id: None,
        });
    }
    establish_local_identity(ctx, account)
}

pub fn change_initial_password(
    ctx: &Ctx,
    email: String,
    current_password: String,
    new_password: String,
) -> Result<SessionStatus> {
    if new_password.chars().count() < 6 {
        return Err(command_error("the new password must contain at least 6 characters"));
    }
    if current_password == new_password {
        return Err(command_error(
            "choose a new password different from the temporary password",
        ));
    }
    let current_password = Zeroizing::new(current_password);
    let new_password = Zeroizing::new(new_password);
    let account = authenticate_local(&ctx.state, &email, &current_password)?;
    update_local_password(&ctx.state, &account.email, &new_password)?;
    establish_local_identity(ctx, account)
}

fn establish_local_identity(ctx: &Ctx, account: LocalAccount) -> Result<SessionStatus> {
    let account_id = account.id;
    let is_admin = account.role == UserRole::Admin;
    let existing = ctx.identity.load()
        .ok()
        .filter(|identity| identity.account_id == account_id);
    let (device_id, device_key_id, device_private_key, device_public_key, trusted_signing_keys) = match existing {
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
    ensure_local_profile(&ctx.state, &account_id, &account.email, &account.display_name)?;
    ctx.identity.save(&StoredIdentity {
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
        is_admin,
        account_id: Some(account_id),
        email: Some(account.email),
        device_key_id: Some(device_key_id),
    })
}

/// Writes or adopts the roster at startup. Hashing two dozen passwords takes
/// a moment, and doing it here keeps it off the first command the sign-in
/// screen makes.
pub fn ensure_local_auth(state: &AppState) {
    match load_local_auth(state) {
        Ok(auth) => tracing::info!(accounts = auth.users.len(), "local credential file ready"),
        Err(error) => tracing::warn!(%error.message, "could not prepare the local credential file"),
    }
}

// --- user administration --------------------------------------------------

/// Reads the roster for the admin panel. Members never see this list.
pub fn list_local_users(ctx: &Ctx) -> Result<Vec<LocalUser>> {
    let (auth, _) = require_admin(ctx)?;
    Ok(local_user_rows(&auth))
}

pub fn create_local_user(ctx: &Ctx, user: NewLocalUser) -> Result<Vec<LocalUser>> {
    let (mut auth, _) = require_admin(ctx)?;
    let email = clean_email(&user.email)?;
    let display_name = clean_display_name(&user.display_name)?;
    if auth
        .users
        .iter()
        .any(|existing| existing.email.eq_ignore_ascii_case(&email))
    {
        return Err(command_error("an account with that email already exists"));
    }
    let password = Zeroizing::new(user.password.unwrap_or_else(|| SEED_PASSWORD.to_owned()));
    check_password(&password)?;
    auth.users.push(LocalCredential {
        id: Some(Uuid::new_v4().to_string()),
        email,
        display_name,
        password_hash: hash_password(&password)?,
        enabled: true,
        must_change_password: ENFORCE_PASSWORD_CHANGE,
        role: user.role,
    });
    save_roster(&ctx.state, auth)
}

/// Renames an account, changes its role, or enables and disables it.
pub fn update_local_user(ctx: &Ctx, user: LocalUserUpdate) -> Result<Vec<LocalUser>> {
    let (mut auth, signed_in) = require_admin(ctx)?;
    let email = clean_email(&user.email)?;
    let display_name = clean_display_name(&user.display_name)?;
    let is_self = email.eq_ignore_ascii_case(&signed_in);
    if is_self && (user.role != UserRole::Admin || !user.enabled) {
        return Err(command_error("you cannot remove your own administrator access"));
    }
    let record = auth
        .users
        .iter_mut()
        .find(|record| record.email.eq_ignore_ascii_case(&email))
        .ok_or_else(|| command_error("that account no longer exists"))?;
    record.display_name = display_name;
    record.role = user.role;
    record.enabled = user.enabled;
    require_remaining_admin(&auth)?;
    save_roster(&ctx.state, auth)
}

/// Sets a new password for another account. The member is not asked to change
/// it while the testing password policy is muted.
pub fn reset_local_user_password(
    ctx: &Ctx,
    email: String,
    password: String,
) -> Result<Vec<LocalUser>> {
    let (mut auth, _) = require_admin(ctx)?;
    let email = clean_email(&email)?;
    let password = Zeroizing::new(password);
    check_password(&password)?;
    let hash = hash_password(&password)?;
    let record = auth
        .users
        .iter_mut()
        .find(|record| record.email.eq_ignore_ascii_case(&email))
        .ok_or_else(|| command_error("that account no longer exists"))?;
    record.password_hash = hash;
    record.must_change_password = ENFORCE_PASSWORD_CHANGE;
    save_roster(&ctx.state, auth)
}

pub fn delete_local_user(ctx: &Ctx, email: String) -> Result<Vec<LocalUser>> {
    let (mut auth, signed_in) = require_admin(ctx)?;
    let email = clean_email(&email)?;
    if email.eq_ignore_ascii_case(&signed_in) {
        return Err(command_error("you cannot remove the account you are signed in with"));
    }
    let before = auth.users.len();
    auth.users.retain(|record| !record.email.eq_ignore_ascii_case(&email));
    if auth.users.len() == before {
        return Err(command_error("that account no longer exists"));
    }
    require_remaining_admin(&auth)?;
    save_roster(&ctx.state, auth)
}

/// Loads the roster and confirms the signed-in account may administer it.
/// Returns the roster and the signed-in email so callers can protect it.
fn require_admin(ctx: &Ctx) -> Result<(LocalAuthFile, String)> {
    let identity = ctx.identity.load().map_err(|_| CommandError::unauthorized("sign in to manage users"))?;
    let auth = load_local_auth(&ctx.state)?;
    let is_admin = auth
        .users
        .iter()
        .any(|user| user.enabled && user.role == UserRole::Admin && user.email.eq_ignore_ascii_case(&identity.email));
    if !is_admin {
        return Err(CommandError::forbidden("only an administrator can manage users"));
    }
    Ok((auth, identity.email))
}

fn require_remaining_admin(auth: &LocalAuthFile) -> Result<()> {
    if auth
        .users
        .iter()
        .any(|user| user.enabled && user.role == UserRole::Admin)
    {
        return Ok(());
    }
    Err(command_error(
        "the workspace must keep at least one enabled administrator",
    ))
}

fn save_roster(state: &AppState, auth: LocalAuthFile) -> Result<Vec<LocalUser>> {
    write_local_auth(&auth_file_path(state), &auth)?;
    Ok(local_user_rows(&auth))
}

fn local_user_rows(auth: &LocalAuthFile) -> Vec<LocalUser> {
    let mut rows: Vec<LocalUser> = auth
        .users
        .iter()
        .map(|user| LocalUser {
            id: credential_id(user),
            email: user.email.trim().to_lowercase(),
            display_name: user.display_name.clone(),
            role: user.role,
            enabled: user.enabled,
            must_change_password: user.must_change_password,
        })
        .collect();
    rows.sort_by_key(|row| row.display_name.to_lowercase());
    rows
}

fn clean_email(email: &str) -> Result<String> {
    let email = email.trim().to_lowercase();
    let valid = email.len() <= 254
        && !email.starts_with('@')
        && !email.ends_with('@')
        && email.matches('@').count() == 1
        && email.split('@').nth(1).is_some_and(|domain| domain.contains('.'))
        && !email.contains(char::is_whitespace);
    if !valid {
        return Err(command_error("enter a valid email address"));
    }
    Ok(email)
}

fn clean_display_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(command_error("display name must contain between 1 and 80 characters"));
    }
    Ok(name.to_owned())
}

fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < 6 {
        return Err(command_error("the password must contain at least 6 characters"));
    }
    Ok(())
}

pub fn clear_authenticated_session(ctx: &Ctx) -> Result<()> {
    ctx.identity.clear()
}

pub fn sign_out_skwad(ctx: &Ctx) -> Result<()> {
    ctx.state.loaded_catalogues.lock().clear();
    clear_authenticated_session(ctx)
}

pub fn get_user_profile(ctx: &Ctx) -> Result<UserProfile> {
    let identity = ctx.identity.load().map_err(|_| command_error("sign in to view your profile"))?;
    load_local_profile(&ctx.state, &identity)
}

pub fn update_user_profile(ctx: &Ctx, update: ProfileUpdate) -> Result<UserProfile> {
    let identity = ctx.identity.load().map_err(|_| command_error("sign in to update your profile"))?;

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
    let previous = load_local_profile(&ctx.state, &identity)?;
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
    save_local_profile(&ctx.state, &profile)?;
    Ok(profile)
}

pub fn publish_skwad(
    ctx: &Ctx,
    shoot_id: i64,
    destination: String,
    passphrase: String,
) -> Result<PublishResult> {
    let mut identity =
        ctx.identity.load().map_err(|_| command_error("sign in once before publishing a SKWAD catalogue"))?;
    let keys = fetch_backend_keys()?;
    identity
        .trusted_signing_keys
        .insert(keys.signing_key_id.clone(), keys.signing_public_key.clone());
    ctx.identity.save(&identity)?;
    let device = identity.device_key()?;
    let backend_public = B64.decode(&keys.wrapping_public_key).map_err(command_error)?;
    let package_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let db = ctx.state.db.clone();
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
        (move || -> Result<(Vec<u8>, u64, i64, String, String, String)> {
            let mut conn = db.conn().map_err(command_error)?;
            let revision_number: i64 = conn
                .row_one(
                    "SELECT coalesce(max(revision_number),0)+1 FROM catalogue_revisions WHERE shoot_id=$1",
                    skwad_database::params![shoot_id],
                )
                .map_err(command_error)?
                .get(0);
            let portable =
                build_portable_catalogue(&mut conn, shoot_id, revision_number as u64).map_err(command_error)?;
            let shoot_name: String = conn
                .row_one("SELECT name FROM shoots WHERE id=$1", skwad_database::params![shoot_id])
                .map_err(command_error)?
                .get(0);
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
        })()?;

    let signed = reqwest::blocking::Client::new()
        .post(format!("{}/v1/packages/sign", backend_url()))
        .bearer_auth(backend_auth_token()?)
        .header("x-skwad-workspace-id", &identity.workspace_id)
        .header(reqwest::header::CONTENT_TYPE, "application/vnd.skwad.catalogue")
        .body(unsigned.0)
        .send()
        .map_err(command_error)?;
    if !signed.status().is_success() {
        return Err(command_error(format!(
            "backend refused publication: {}",
            signed.text().unwrap_or_default()
        )));
    }
    let package = signed.bytes().map_err(command_error)?;
    let destination = package_destination(&destination);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(command_error)?;
    }
    std::fs::write(&destination, &package).map_err(command_error)?;
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    conn.exec("INSERT INTO catalogue_revisions(revision_id,package_id,shoot_id,revision_number,state,manifest_hash,created_at,published_at) VALUES($1,$2,$3,$4,'published',$5,$6,$6)", skwad_database::params![revision_id.to_string(), package_id.to_string(), shoot_id, unsigned.2, blake3::hash(&package).to_hex().to_string(), chrono::Utc::now().to_rfc3339()]).map_err(command_error)?;
    Ok(PublishResult {
        package_id: package_id.to_string(),
        revision_id: revision_id.to_string(),
        path: destination.to_string_lossy().into_owned(),
        media_count: unsigned.1,
    })
}

pub fn load_skwad(
    ctx: &Ctx,
    path: String,
    passphrase: Option<String>,
) -> Result<LoadedCatalogueInfo> {
    let identity = ctx.identity.load()
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
    ctx.state.db.conn().map_err(command_error)?.exec("INSERT INTO imported_catalogues(package_id,revision_id,library_id,shoot_id,catalogue_hash,imported_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(package_id,revision_id) DO UPDATE SET imported_at=excluded.imported_at", skwad_database::params![header.authenticated.package_id.to_string(), header.authenticated.revision_id.to_string(), summary.library_id, summary.shoot_id, blake3::hash(&package).to_hex().to_string(), chrono::Utc::now().to_rfc3339()]).map_err(command_error)?;
    let key = catalogue_key(
        &header.authenticated.package_id.to_string(),
        &header.authenticated.revision_id.to_string(),
    );
    ctx.state.loaded_catalogues.lock().insert(
        key,
        LoadedCatalogue {
            package_id: header.authenticated.package_id.to_string(),
            revision_id: header.authenticated.revision_id.to_string(),
            summary: summary.clone(),
            catalogue: decoded.catalogue,
        },
    );
    loaded_info(
        &ctx.state,
        &header.authenticated.package_id.to_string(),
        &header.authenticated.revision_id.to_string(),
        summary,
    )
}

pub fn approve_catalogue_library(
    ctx: &Ctx,
    package_id: String,
    revision_id: String,
    root: String,
) -> Result<LoadedCatalogueInfo> {
    let path = PathBuf::from(&root);
    if !path.is_dir() {
        return Err(command_error("choose an existing NAS library folder"));
    }
    let key = catalogue_key(&package_id, &revision_id);
    let loaded = ctx.state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&key)
        .ok_or_else(|| command_error("load the catalogue before mapping its library"))?;
    let now = chrono::Utc::now().to_rfc3339();
    ctx.state.db.conn().map_err(command_error)?.exec("INSERT INTO library_mappings(library_id,label,local_root,approved_at,updated_at) VALUES($1,$2,$3,$4,$4) ON CONFLICT(library_id) DO UPDATE SET local_root=excluded.local_root,updated_at=excluded.updated_at", skwad_database::params![catalogue.summary.library_id, catalogue.summary.shoot_name, path.to_string_lossy(), now]).map_err(command_error)?;
    loaded_info(&ctx.state, &package_id, &revision_id, catalogue.summary.clone())
}

pub fn list_loaded_catalogues(ctx: &Ctx) -> Result<Vec<LoadedCatalogueInfo>> {
    ctx.state
        .loaded_catalogues
        .lock()
        .values()
        .map(|catalogue| {
            loaded_info(
                &ctx.state,
                &catalogue.package_id,
                &catalogue.revision_id,
                catalogue.summary.clone(),
            )
        })
        .collect()
}

pub fn list_catalogue_groups(
    ctx: &Ctx,
    package_id: String,
    revision_id: String,
) -> Result<Vec<CatalogueGroup>> {
    let loaded = ctx.state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&catalogue_key(&package_id, &revision_id))
        .ok_or_else(|| command_error("catalogue is not loaded"))?;
    catalogue_groups(&catalogue.catalogue).map_err(command_error)
}

pub fn list_catalogue_media(
    ctx: &Ctx,
    package_id: String,
    revision_id: String,
    group_id: Option<i64>,
) -> Result<Vec<CatalogueMedia>> {
    let loaded = ctx.state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&catalogue_key(&package_id, &revision_id))
        .ok_or_else(|| command_error("catalogue is not loaded"))?;
    catalogue_media(&catalogue.catalogue, group_id).map_err(command_error)
}

/// Resolves one catalogue media reference to a file under the approved local
/// root, checking it cannot escape that root. Opening it is the front end's
/// job — this is the machine-specific half a headless server cannot do.
pub fn resolve_catalogue_media(
    ctx: &Ctx,
    package_id: String,
    revision_id: String,
    media_id: i64,
) -> Result<String> {
    let loaded = ctx.state.loaded_catalogues.lock();
    let catalogue = loaded
        .get(&catalogue_key(&package_id, &revision_id))
        .ok_or_else(|| command_error("catalogue is not loaded"))?;
    let media = catalogue_media(&catalogue.catalogue, None)
        .map_err(command_error)?
        .into_iter()
        .find(|item| item.id == media_id)
        .ok_or_else(|| command_error("media reference is missing"))?;
    let root: String = ctx.state
        .db
        .conn()
        .map_err(command_error)?
        .row_opt(
            "SELECT local_root FROM library_mappings WHERE library_id=$1",
            skwad_database::params![catalogue.summary.library_id],
        )
        .ok()
        .flatten()
        .map(|row| row.get(0))
        .ok_or_else(|| command_error("map this catalogue to an approved NAS root first"))?;
    let target = resolve_beneath_root(Path::new(&root), &media.relative_path).map_err(command_error)?;
    let canonical_root = std::fs::canonicalize(&root).map_err(command_error)?;
    let canonical_target = std::fs::canonicalize(&target).map_err(command_error)?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(command_error("media reference resolves outside the approved NAS root"));
    }
    Ok(canonical_target.to_string_lossy().into_owned())
}

fn loaded_info(
    state: &AppState,
    package_id: &str,
    revision_id: &str,
    summary: CatalogueSummary,
) -> Result<LoadedCatalogueInfo> {
    let mapped_root: Option<String> = state
        .db
        .conn()
        .map_err(command_error)?
        .row_opt(
            "SELECT local_root FROM library_mappings WHERE library_id=$1",
            skwad_database::params![summary.library_id],
        )
        .ok()
        .flatten()
        .map(|row| row.get(0));
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
    if !path.exists() {
        seed_local_auth(&path)?;
    }
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
    let mut auth: LocalAuthFile = serde_json::from_slice(&bytes)
        .map_err(|error| command_error(format!("local credential file is invalid: {error}")))?;
    if auth.version != LOCAL_AUTH_VERSION {
        return Err(command_error(format!(
            "unsupported local credential file version {}",
            auth.version
        )));
    }
    // A file written before roles existed has nobody who can open the admin
    // panel, so the roster is adopted once. After that the panel owns the file
    // and accounts an administrator removed stay removed.
    if !auth.users.iter().any(|user| user.role == UserRole::Admin) && adopt_seed_roster(&mut auth)? {
        write_local_auth(&path, &auth)?;
    }
    Ok(auth)
}

/// Adds the seeded roster to a credential file that predates roles, keeping
/// the passwords of accounts that are already there. Answers whether anything
/// changed.
fn adopt_seed_roster(auth: &mut LocalAuthFile) -> Result<bool> {
    let mut changed = false;
    for (email, display_name, role) in SEED_USERS {
        match auth
            .users
            .iter_mut()
            .find(|user| user.email.eq_ignore_ascii_case(email))
        {
            Some(existing) => {
                if existing.role != *role || existing.must_change_password != ENFORCE_PASSWORD_CHANGE {
                    existing.role = *role;
                    existing.must_change_password = ENFORCE_PASSWORD_CHANGE;
                    changed = true;
                }
            }
            None => {
                auth.users.push(LocalCredential {
                    id: Some(Uuid::new_v4().to_string()),
                    email: (*email).to_owned(),
                    display_name: (*display_name).to_owned(),
                    password_hash: hash_password(SEED_PASSWORD)?,
                    enabled: true,
                    must_change_password: ENFORCE_PASSWORD_CHANGE,
                    role: *role,
                });
                changed = true;
            }
        }
    }
    Ok(changed)
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
    let id = credential_id(user);
    let display_name = user.display_name.trim();
    if canonical_email.is_empty() || display_name.is_empty() || display_name.chars().count() > 80 {
        return Err(command_error("the credential file contains an invalid user record"));
    }
    Ok(LocalAccount {
        id,
        email: canonical_email,
        display_name: display_name.to_owned(),
        role: user.role,
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
    write_local_auth(&path, &auth)
}

fn write_local_auth(path: &Path, auth: &LocalAuthFile) -> Result<()> {
    let encoded = serde_json::to_vec_pretty(auth).map_err(command_error)?;
    let parent = path
        .parent()
        .ok_or_else(|| command_error("credential file path has no parent directory"))?;
    std::fs::create_dir_all(parent).map_err(command_error)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, &encoded).map_err(command_error)?;
    std::fs::copy(&temporary, path).map_err(command_error)?;
    let _ = std::fs::remove_file(temporary);
    Ok(())
}

fn credential_id(user: &LocalCredential) -> String {
    user.id.clone().filter(|id| !id.trim().is_empty()).unwrap_or_else(|| {
        let canonical = user.email.trim().to_lowercase();
        format!("local-{}", &blake3::hash(canonical.as_bytes()).to_hex()[..32])
    })
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(command_error)?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(command_error)?
        .to_string())
}

/// Writes the team roster the first time the app runs on a machine, so the
/// build is testable without running the provisioning binary by hand.
fn seed_local_auth(path: &Path) -> Result<()> {
    let users = SEED_USERS
        .iter()
        .map(|(email, display_name, role)| {
            Ok(LocalCredential {
                id: Some(Uuid::new_v4().to_string()),
                email: (*email).to_owned(),
                display_name: (*display_name).to_owned(),
                password_hash: hash_password(SEED_PASSWORD)?,
                enabled: true,
                must_change_password: ENFORCE_PASSWORD_CHANGE,
                role: *role,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    write_local_auth(
        path,
        &LocalAuthFile {
            version: LOCAL_AUTH_VERSION,
            users,
        },
    )
}

fn profile_key(account_id: &str) -> String {
    format!("{PROFILE_KEY_PREFIX}{account_id}")
}

fn ensure_local_profile(state: &AppState, account_id: &str, email: &str, display_name: &str) -> Result<()> {
    let mut conn = state.db.conn().map_err(command_error)?;
    let key = profile_key(account_id);
    if skwad_database::repo::settings::get_raw(&mut conn, &key)
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
    skwad_database::repo::settings::set(&mut conn, &key, &profile).map_err(command_error)
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
    let mut conn = state.db.conn().map_err(command_error)?;
    skwad_database::repo::settings::get_raw(&mut conn, &profile_key(&identity.account_id))
        .map_err(command_error)?
        .ok_or_else(|| command_error("the local profile was not found"))
        .and_then(|value| serde_json::from_str(&value).map_err(command_error))
}

fn save_local_profile(state: &AppState, profile: &UserProfile) -> Result<()> {
    let mut conn = state.db.conn().map_err(command_error)?;
    skwad_database::repo::settings::set(&mut conn, &profile_key(&profile.user_id), profile).map_err(command_error)
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

fn fetch_backend_keys() -> Result<BackendKeys> {
    reqwest::blocking::Client::new()
        .get(format!("{}/v1/crypto/public-keys", backend_url()))
        .send()
        .map_err(command_error)?
        .error_for_status()
        .map_err(command_error)?
        .json()
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
fn command_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::bad_request(error.to_string())
}

impl StoredIdentity {
    pub fn device_key(&self) -> Result<DeviceKeyPair> {
        DeviceKeyPair::from_bytes(
            &self.device_key_id,
            B64.decode(&self.device_private_key).map_err(command_error)?,
            B64.decode(&self.device_public_key).map_err(command_error)?,
        )
        .map_err(command_error)
    }
    pub fn verifying_key(&self, id: &str) -> Result<[u8; 32]> {
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
        auth_file_path, authenticate_local, check_password, clean_display_name, clean_email, clean_optional,
        load_local_auth, require_remaining_admin, update_local_password, write_local_auth, LocalAuthFile,
        LocalCredential, UserRole, SEED_PASSWORD, SEED_USERS,
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
                role: UserRole::Member,
            }],
        };
        std::fs::write(
            auth_dir.join("credentials.json"),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            Database::open_test().unwrap(),
            paths,
            AppSettings::default(),
            "skwadmedia://".into(),
            "test-machine",
            temp.path().join("machine-settings.json"),
        ));

        assert!(authenticate_local(&state, "PERSON@example.com", "temporary-password").is_ok());
        assert!(authenticate_local(&state, "person@example.com", "wrong-password").is_err());
        update_local_password(&state, "person@example.com", "a-new-private-password").unwrap();
        assert!(authenticate_local(&state, "person@example.com", "temporary-password").is_err());
        assert!(authenticate_local(&state, "person@example.com", "a-new-private-password").is_ok());
        assert!(!load_local_auth(&state).unwrap().users[0].must_change_password);
    }

    #[test]
    fn a_missing_credential_file_is_seeded_with_the_team_roster() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::create(temp.path()).unwrap();
        let state = Arc::new(AppState::new(
            Database::open_test().unwrap(),
            paths,
            AppSettings::default(),
            "skwadmedia://".into(),
            "test-machine",
            temp.path().join("machine-settings.json"),
        ));

        let auth = load_local_auth(&state).unwrap();
        assert_eq!(auth.users.len(), SEED_USERS.len());
        assert!(auth.users.iter().all(|user| user.enabled && !user.must_change_password));
        let admins: Vec<&str> = auth
            .users
            .iter()
            .filter(|user| user.role == UserRole::Admin)
            .map(|user| user.email.as_str())
            .collect();
        assert_eq!(admins, ["naresh.nallamothu@tesseractesports.com"]);

        // The seeded password signs in, and no first-change prompt follows it.
        let account = authenticate_local(&state, "NARESH.NALLAMOTHU@tesseractesports.com", SEED_PASSWORD).unwrap();
        assert_eq!(account.role, UserRole::Admin);
        assert!(authenticate_local(&state, "yash.patle@tesseractesports.com", SEED_PASSWORD).is_ok());
        assert!(authenticate_local(&state, "yash.patle@tesseractesports.com", "wrong").is_err());
    }

    #[test]
    fn a_credential_file_without_an_administrator_adopts_the_roster_once() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::create(temp.path()).unwrap();
        let auth_dir = paths.root.join("auth");
        std::fs::create_dir_all(&auth_dir).unwrap();
        let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).unwrap();
        let hash = Argon2::default()
            .hash_password(b"their-own-password", &salt)
            .unwrap()
            .to_string();
        let document = LocalAuthFile {
            version: 1,
            users: vec![LocalCredential {
                id: Some("local-test-user".into()),
                email: "yash.patle@tesseractesports.com".into(),
                display_name: "Yash Patle".into(),
                password_hash: hash,
                enabled: true,
                must_change_password: true,
                role: UserRole::Member,
            }],
        };
        std::fs::write(
            auth_dir.join("credentials.json"),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            Database::open_test().unwrap(),
            paths,
            AppSettings::default(),
            "skwadmedia://".into(),
            "test-machine",
            temp.path().join("machine-settings.json"),
        ));

        let auth = load_local_auth(&state).unwrap();
        assert_eq!(auth.users.len(), SEED_USERS.len());
        // The account that was already there keeps its own password.
        assert!(authenticate_local(&state, "yash.patle@tesseractesports.com", "their-own-password").is_ok());
        assert!(authenticate_local(&state, "naresh.nallamothu@tesseractesports.com", SEED_PASSWORD).is_ok());

        // An administrator removing somebody sticks: the roster is not re-adopted.
        let mut auth = load_local_auth(&state).unwrap();
        auth.users
            .retain(|user| !user.email.eq_ignore_ascii_case("yash.patle@tesseractesports.com"));
        write_local_auth(&auth_file_path(&state), &auth).unwrap();
        assert_eq!(load_local_auth(&state).unwrap().users.len(), SEED_USERS.len() - 1);
    }

    #[test]
    fn credential_records_without_a_role_are_members() {
        let json = r#"{"version":1,"users":[{"email":"person@example.com","displayName":"Person","passwordHash":"hash","enabled":true}]}"#;
        let auth: LocalAuthFile = serde_json::from_str(json).unwrap();
        assert_eq!(auth.users[0].role, UserRole::Member);
    }

    #[test]
    fn the_roster_keeps_an_enabled_administrator() {
        let mut auth = LocalAuthFile {
            version: 1,
            users: vec![LocalCredential {
                id: None,
                email: "person@example.com".into(),
                display_name: "Person".into(),
                password_hash: "hash".into(),
                enabled: true,
                must_change_password: false,
                role: UserRole::Admin,
            }],
        };
        assert!(require_remaining_admin(&auth).is_ok());
        auth.users[0].role = UserRole::Member;
        assert!(require_remaining_admin(&auth).is_err());
    }

    #[test]
    fn admin_input_is_validated() {
        assert_eq!(clean_email(" Person@Example.COM ").unwrap(), "person@example.com");
        assert!(clean_email("person@example").is_err());
        assert!(clean_email("person.example.com").is_err());
        assert!(clean_display_name("  ").is_err());
        assert!(check_password("12345").is_err());
        assert!(check_password("Tess@123").is_ok());
    }

    #[test]
    fn plaintext_password_fields_are_rejected() {
        let json = r#"{"version":1,"users":[{"email":"person@example.com","displayName":"Person","password":"unsafe","passwordHash":"hash","enabled":true}]}"#;
        assert!(serde_json::from_str::<LocalAuthFile>(json).is_err());
    }
}
