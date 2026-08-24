//! Bearer-token auth over `/api/*` and `/media/*`.
//!
//! One shared secret, because that is what the deployment actually is: a LAN
//! service for a small edit team, not a public site. The token comes from
//! `TEO_TOKEN` or is generated on first run and written to `<data>/token`.
//!
//! Three ways to present it, all checked in constant time:
//!
//! * `Authorization: Bearer <token>` — for `fetch` and for `curl`.
//! * a `teo_token` cookie — because `<img>` and `<video>` tags cannot send
//!   headers, and media has to load in the browser without rewriting every URL
//!   into a signed one.
//! * `?token=…` — the same problem again, but cross-origin, where the cookie
//!   cannot help: the desktop shell's webview is `tauri.localhost` talking to
//!   `127.0.0.1`, so a `SameSite` cookie is never sent. That case is loopback
//!   only with a token that lives for one launch, which is why a token in a URL
//!   is acceptable there and avoided everywhere else.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::RngCore;

use crate::error::ApiError;
use crate::state::ServerState;

const COOKIE_NAME: &str = "teo_token";

/// Loads the configured token, or generates and persists one.
///
/// The file is written `0600` on Unix. Windows has no mode bits, so the file
/// inherits the data directory's ACL — a container or a NAS share is the
/// expected home for this, and the directory is the thing to lock down there.
pub fn resolve_token(configured: Option<String>, data_dir: &std::path::Path) -> std::io::Result<String> {
    if let Some(token) = configured.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        return Ok(token);
    }

    let path = data_dir.join("token");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }

    let token = generate_token();
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(&path, format!("{token}\n"))?;
    restrict_permissions(&path)?;
    tracing::info!(path = %path.display(), "generated an access token");
    Ok(token)
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Length-independent comparison. The length itself is not secret, so an early
/// return on a mismatched length is fine; the bytes are compared without one.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Pulls a token off the request: header first, then cookie, then query.
fn presented_token<B>(request: &Request<B>) -> Option<String> {
    if let Some(value) = request.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) {
            return Some(token.trim().to_string());
        }
    }

    if let Some(cookies) = request.headers().get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        let found = cookies.split(';').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name.trim() == COOKIE_NAME).then(|| value.trim().to_string())
        });
        if found.is_some() {
            return found;
        }
    }

    // Last resort for a client that can send neither: `EventSource` and media
    // tags, cross-origin.
    request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == "token").then(|| urldecode(value))
        })
    })
}

/// Percent-decoding, limited to what a token can contain: hex and the odd
/// character a configured token might carry.
fn urldecode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Rejects anything that does not present the token.
pub async fn require_token(
    State(state): State<Arc<ServerState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let presented = presented_token(&request);
    let authorised = presented
        .as_deref()
        .is_some_and(|token| constant_time_eq(token.as_bytes(), state.token.as_bytes()));

    if !authorised {
        return ApiError::unauthorized(if presented.is_some() {
            "that token is not valid for this server"
        } else {
            "a bearer token is required"
        })
        .into_response();
    }

    next.run(request).await
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest {
    pub token: String,
}

/// Trades a token for a cookie, so media tags load without header injection.
///
/// Deliberately not behind the middleware: this is where a browser presents the
/// token for the first time.
pub async fn create_session(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<SessionRequest>,
) -> Response {
    if !constant_time_eq(body.token.trim().as_bytes(), state.token.as_bytes()) {
        return ApiError::unauthorized("that token is not valid for this server").into_response();
    }

    // `SameSite=Strict` because every legitimate request is same-origin — the
    // bundle is served by this server. No `Secure`: a LAN deployment over plain
    // HTTP would otherwise never receive the cookie back.
    let cookie = format!(
        "{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        state.token,
        60 * 60 * 24 * 30
    );

    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_token_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let token = resolve_token(Some("  from-env  ".into()), dir.path()).unwrap();
        assert_eq!(token, "from-env");
        assert!(!dir.path().join("token").exists(), "nothing to persist when it was supplied");
    }

    #[test]
    fn a_generated_token_is_persisted_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let first = resolve_token(None, dir.path()).unwrap();
        assert_eq!(first.len(), 64, "32 random bytes, hex encoded");

        let second = resolve_token(None, dir.path()).unwrap();
        assert_eq!(first, second, "a restart must not lock out existing clients");
    }

    #[test]
    fn a_blank_configured_token_falls_back_to_generation() {
        let dir = tempfile::tempdir().unwrap();
        let token = resolve_token(Some("   ".into()), dir.path()).unwrap();
        assert_eq!(token.len(), 64);
        assert!(dir.path().join("token").exists());
    }

    #[test]
    fn comparison_rejects_near_misses() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_token_can_arrive_as_a_header_or_a_cookie() {
        let with_header = Request::builder()
            .header(header::AUTHORIZATION, "Bearer secret-value")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&with_header).as_deref(), Some("secret-value"));

        let lowercase = Request::builder()
            .header(header::AUTHORIZATION, "bearer secret-value")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&lowercase).as_deref(), Some("secret-value"));

        let with_cookie = Request::builder()
            .header(header::COOKIE, "other=1; teo_token=secret-value; another=2")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&with_cookie).as_deref(), Some("secret-value"));

        let without = Request::builder().body(()).unwrap();
        assert_eq!(presented_token(&without), None);

        let wrong_scheme = Request::builder()
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&wrong_scheme), None);
    }

    #[test]
    fn a_query_token_is_the_last_resort() {
        let in_query = Request::builder()
            .uri("/media/7/thumb?token=secret-value")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&in_query).as_deref(), Some("secret-value"));

        // Alongside other parameters, and percent-encoded.
        let messy = Request::builder()
            .uri("/api/events?other=1&token=a%2Fb")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&messy).as_deref(), Some("a/b"));

        // A header still wins, so a stray query parameter cannot downgrade a
        // properly authenticated request.
        let both = Request::builder()
            .uri("/media/7/thumb?token=from-query")
            .header(header::AUTHORIZATION, "Bearer from-header")
            .body(())
            .unwrap();
        assert_eq!(presented_token(&both).as_deref(), Some("from-header"));
    }
}
