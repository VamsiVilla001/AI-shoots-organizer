//! Per-user sessions over the existing Argon2id accounts.
//!
//! The desktop remembers who signed in by keeping their device identity in
//! the operating-system credential store. A server has many users, so it keeps
//! one identity file per account under the library's `auth` folder and hands
//! each browser or client a session token instead.
//!
//! A token is presented one of three ways, in this order:
//!
//! * `Authorization: Bearer <token>` — `fetch` from any client.
//! * a `skwad_session` cookie — set at sign-in so `<img>`, `<video>` and
//!   `EventSource`, which cannot send headers, still authenticate from a page
//!   this server served.
//! * `?token=<token>` — for those same three from a page served elsewhere
//!   (the desktop client's webview), where a cookie is never sent.
//!
//! Sessions are kept in memory and mirrored to `auth/sessions.json` so a
//! restart does not sign everybody out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::Request;
use axum::http::header;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use skwad_app_core::api::{ApiError, IdentityStore, SessionUser, StoredIdentity};

pub const COOKIE_NAME: &str = "skwad_session";
pub const SESSION_HEADER: &str = "x-skwad-session-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub token: String,
    pub account_id: String,
    pub email: String,
    pub display_name: String,
    pub created_at: u64,
    pub last_seen: u64,
}

impl SessionRecord {
    pub fn user(&self) -> SessionUser {
        SessionUser {
            account_id: self.account_id.clone(),
            email: self.email.clone(),
            display_name: self.display_name.clone(),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn new_token() -> String {
    // Two v4 UUIDs: 256 bits of operating-system randomness, hex.
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

pub struct SessionStore {
    sessions: Mutex<HashMap<String, SessionRecord>>,
    path: PathBuf,
    ttl: Duration,
}

impl SessionStore {
    pub fn open(auth_dir: &Path, ttl: Duration) -> Self {
        let path = auth_dir.join("sessions.json");
        let sessions: HashMap<String, SessionRecord> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<SessionRecord>>(&bytes).ok())
            .map(|list| list.into_iter().map(|s| (s.token.clone(), s)).collect())
            .unwrap_or_default();
        let store = Self {
            sessions: Mutex::new(sessions),
            path,
            ttl,
        };
        store.sweep();
        store
    }

    fn persist(&self, sessions: &HashMap<String, SessionRecord>) {
        let list: Vec<&SessionRecord> = sessions.values().collect();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_vec_pretty(&list) {
            Ok(json) => {
                if let Err(error) = std::fs::write(&self.path, json) {
                    tracing::warn!(%error, path = %self.path.display(), "could not persist sessions");
                }
            }
            Err(error) => tracing::warn!(%error, "could not serialise sessions"),
        }
    }

    /// Drops sessions idle for longer than the TTL.
    pub fn sweep(&self) {
        let mut sessions = self.sessions.lock();
        let cutoff = now_secs().saturating_sub(self.ttl.as_secs());
        let before = sessions.len();
        sessions.retain(|_, s| s.last_seen >= cutoff);
        if sessions.len() != before {
            self.persist(&sessions);
        }
    }

    pub fn create(&self, user: SessionUser) -> SessionRecord {
        let now = now_secs();
        let record = SessionRecord {
            token: new_token(),
            account_id: user.account_id,
            email: user.email,
            display_name: user.display_name,
            created_at: now,
            last_seen: now,
        };
        let mut sessions = self.sessions.lock();
        sessions.insert(record.token.clone(), record.clone());
        self.persist(&sessions);
        record
    }

    /// The session behind a token, if it exists and has not idled out.
    pub fn resolve(&self, token: &str) -> Option<SessionRecord> {
        let mut sessions = self.sessions.lock();
        let cutoff = now_secs().saturating_sub(self.ttl.as_secs());
        let record = sessions.get_mut(token)?;
        if record.last_seen < cutoff {
            sessions.remove(token);
            self.persist(&sessions);
            return None;
        }
        // Touch at most once a minute, so a busy grid does not rewrite the
        // file on every thumbnail.
        let now = now_secs();
        if now.saturating_sub(record.last_seen) > 60 {
            record.last_seen = now;
            let snapshot = record.clone();
            self.persist(&sessions);
            return Some(snapshot);
        }
        Some(record.clone())
    }

    pub fn revoke(&self, token: &str) {
        let mut sessions = self.sessions.lock();
        if sessions.remove(token).is_some() {
            self.persist(&sessions);
        }
    }

    /// Every session of one account, for revoking a disabled user.
    pub fn revoke_account(&self, account_id: &str) {
        let mut sessions = self.sessions.lock();
        let before = sessions.len();
        sessions.retain(|_, s| s.account_id != account_id);
        if sessions.len() != before {
            self.persist(&sessions);
        }
    }

    pub fn count(&self) -> usize {
        self.sessions.lock().len()
    }
}

/// The token a request carries, if any: header, then cookie, then query.
pub fn presented_token(request: &Request) -> Option<String> {
    if let Some(value) = request.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    if let Some(cookies) = request.headers().get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        let found = cookies.split(';').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name.trim() == COOKIE_NAME).then(|| value.trim().to_string())
        });
        if found.as_deref().is_some_and(|t| !t.is_empty()) {
            return found;
        }
    }
    request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == "token" && !value.is_empty()).then(|| value.to_string())
        })
    })
}

/// The `Set-Cookie` value for a fresh session. `Secure` only over TLS — on a
/// plain loopback deployment the browser would otherwise never send it back.
pub fn session_cookie(token: &str, ttl: Duration, secure: bool) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        ttl.as_secs(),
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// One account's device identity, as a file under `auth/identities`.
///
/// Holds the account's catalogue keypair, so the directory must be readable
/// by the service account only; `install` sets that ACL. Keyed by a hash of
/// the email rather than the email itself so an address never becomes a file
/// name.
pub struct FileIdentityStore {
    path: PathBuf,
}

impl FileIdentityStore {
    pub fn for_email(auth_dir: &Path, email: &str) -> Self {
        let key = blake3::hash(email.trim().to_lowercase().as_bytes()).to_hex();
        Self {
            path: auth_dir.join("identities").join(format!("{}.json", &key[..32])),
        }
    }
}

impl IdentityStore for FileIdentityStore {
    fn load(&self) -> skwad_app_core::api::Result<StoredIdentity> {
        let bytes = std::fs::read(&self.path).map_err(|_| ApiError::unauthorized("sign in first"))?;
        serde_json::from_slice(&bytes).map_err(|e| ApiError::internal(format!("the identity file is unreadable: {e}")))
    }

    fn save(&self, identity: &StoredIdentity) -> skwad_app_core::api::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }
        let json = serde_json::to_vec_pretty(identity).map_err(|e| ApiError::internal(e.to_string()))?;
        std::fs::write(&self.path, json).map_err(|e| ApiError::internal(e.to_string()))
    }

    fn clear(&self) -> skwad_app_core::api::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    }
}

/// An identity store for a caller that has not signed in and therefore has
/// nothing to load, but which a sign-in may write into once it knows the
/// email. The dispatcher binds this from the request body for sign-in calls.
pub fn store_for_request(auth_dir: &Path, session: Option<&SessionRecord>, email_hint: Option<&str>) -> Arc<dyn IdentityStore> {
    match session.map(|s| s.email.as_str()).or(email_hint) {
        Some(email) => Arc::new(FileIdentityStore::for_email(auth_dir, email)),
        None => Arc::new(skwad_app_core::api::NoIdentity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(email: &str) -> SessionUser {
        SessionUser {
            account_id: format!("acct-{email}"),
            email: email.into(),
            display_name: "Person".into(),
        }
    }

    #[test]
    fn sessions_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), Duration::from_secs(3600));
        let record = store.create(user("a@example.com"));
        assert_eq!(record.token.len(), 64);
        assert_eq!(store.resolve(&record.token).unwrap().email, "a@example.com");

        let reopened = SessionStore::open(dir.path(), Duration::from_secs(3600));
        assert_eq!(reopened.count(), 1, "a restart keeps people signed in");
        reopened.revoke(&record.token);
        assert!(reopened.resolve(&record.token).is_none());
        assert_eq!(SessionStore::open(dir.path(), Duration::from_secs(3600)).count(), 0);
    }

    #[test]
    fn an_idle_session_expires() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), Duration::from_secs(0));
        let record = store.create(user("a@example.com"));
        // TTL zero: anything older than "now" is gone on the next look.
        std::thread::sleep(Duration::from_millis(1100));
        assert!(store.resolve(&record.token).is_none());
    }

    #[test]
    fn a_token_can_arrive_three_ways_and_the_header_wins() {
        let with_header = Request::builder()
            .uri("/api/invoke/x?token=from-query")
            .header(header::AUTHORIZATION, "Bearer from-header")
            .header(header::COOKIE, "skwad_session=from-cookie")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(presented_token(&with_header).as_deref(), Some("from-header"));

        let with_cookie = Request::builder()
            .uri("/media/thumb/7?token=from-query")
            .header(header::COOKIE, "other=1; skwad_session=from-cookie")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(presented_token(&with_cookie).as_deref(), Some("from-cookie"));

        let with_query = Request::builder()
            .uri("/api/events?token=from-query")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(presented_token(&with_query).as_deref(), Some("from-query"));

        let none = Request::builder().body(axum::body::Body::empty()).unwrap();
        assert_eq!(presented_token(&none), None);
    }

    #[test]
    fn identities_are_kept_per_account_and_never_named_by_email() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileIdentityStore::for_email(dir.path(), "Person@Example.com");
        assert!(store.load().is_err(), "nothing yet");
        let identity = StoredIdentity {
            account_id: "acct".into(),
            email: "person@example.com".into(),
            display_name: "Person".into(),
            workspace_id: "w".into(),
            access_token: String::new(),
            refresh_token: String::new(),
            device_id: "d".into(),
            device_key_id: "k".into(),
            device_private_key: "priv".into(),
            device_public_key: "pub".into(),
            trusted_signing_keys: Default::default(),
        };
        store.save(&identity).unwrap();
        let same_person = FileIdentityStore::for_email(dir.path(), "  person@example.com ");
        assert_eq!(same_person.load().unwrap().device_key_id, "k");
        for entry in std::fs::read_dir(dir.path().join("identities")).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.contains('@'), "{name}");
        }
        store.clear().unwrap();
        assert!(same_person.load().is_err());
    }
}
