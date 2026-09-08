use std::{env, sync::Arc};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD as B64, Engine};
use serde::Serialize;
use skwad_catalogue::{
    generate_device_keypair, rewrap_package, sign_prepared_package, validate_prepared_package, DeviceKeyPair,
    Recipient, SigningKeyPair,
};
use tower_http::trace::TraceLayer;

const MAX_PACKAGE_BYTES: usize = 513 * 1024 * 1024;

#[derive(Clone)]
struct BackendState {
    auth_token: String,
    signing: SigningKeyPair,
    wrapping: DeviceKeyPair,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicKeys {
    signing_key_id: String,
    signing_public_key: String,
    wrapping_key_id: String,
    wrapping_public_key: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    if env::args().nth(1).as_deref() == Some("keygen") {
        print_generated_keys();
        return;
    }

    let state = Arc::new(BackendState::from_env().unwrap_or_else(|error| {
        eprintln!("SKWAD backend configuration error: {error}");
        eprintln!("Run `cargo run -p skwad-backend -- keygen` and put the values in server-side secret configuration.");
        std::process::exit(2);
    }));
    let bind = env::var("SKWAD_BACKEND_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind SKWAD backend");
    tracing::info!(%bind, "SKWAD backend listening");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown())
        .await
        .expect("serve SKWAD backend");
}

fn app(state: Arc<BackendState>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/crypto/public-keys", get(public_keys))
        .route("/v1/packages/sign", post(sign_package))
        .route("/v1/packages/rewrap", post(rewrap))
        .layer(DefaultBodyLimit::max(MAX_PACKAGE_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn public_keys(State(state): State<Arc<BackendState>>) -> Json<PublicKeys> {
    Json(PublicKeys {
        signing_key_id: state.signing.key_id.clone(),
        signing_public_key: B64.encode(state.signing.verifying_key_bytes()),
        wrapping_key_id: state.wrapping.key_id.clone(),
        wrapping_public_key: B64.encode(state.wrapping.public_key_bytes()),
    })
}

async fn sign_package(State(state): State<Arc<BackendState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorised_owner(&headers, &state).await {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Err(error) = validate_prepared_package(&body, &state.wrapping) {
        return error_response(error);
    }
    match sign_prepared_package(&body, &state.signing) {
        Ok(package) => ([(header::CONTENT_TYPE, "application/vnd.skwad.catalogue")], package).into_response(),
        Err(error) => error_response(error),
    }
}

async fn rewrap(State(state): State<Arc<BackendState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorised_owner(&headers, &state).await {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(key_id) = header_text(&headers, "x-skwad-recipient-key-id") else {
        return (StatusCode::BAD_REQUEST, "missing recipient key id").into_response();
    };
    let Some(public_key) =
        header_text(&headers, "x-skwad-recipient-public-key").and_then(|value| B64.decode(value).ok())
    else {
        return (StatusCode::BAD_REQUEST, "invalid recipient public key").into_response();
    };
    match rewrap_package(
        &body,
        &state.wrapping,
        Recipient { key_id, public_key },
        &state.signing,
        &state.signing.verifying_key_bytes(),
    ) {
        Ok(package) => ([(header::CONTENT_TYPE, "application/vnd.skwad.catalogue")], package).into_response(),
        Err(error) => error_response(error),
    }
}

async fn authorised_owner(headers: &HeaderMap, state: &BackendState) -> bool {
    let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    bearer == state.auth_token
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}

fn error_response(error: impl std::fmt::Display) -> Response {
    tracing::warn!(%error, "package request rejected");
    (StatusCode::BAD_REQUEST, error.to_string()).into_response()
}

impl BackendState {
    fn from_env() -> Result<Self, String> {
        let auth_token = required("SKWAD_BACKEND_AUTH_TOKEN")?;
        if auth_token.len() < 24 {
            return Err("SKWAD_BACKEND_AUTH_TOKEN must contain at least 24 characters".into());
        }
        let signing_key_id = required("SKWAD_SIGNING_KEY_ID")?;
        let signing_secret = decode_fixed::<32>(&required("SKWAD_SIGNING_PRIVATE_KEY")?)?;
        let wrapping_key_id = required("SKWAD_WRAPPING_KEY_ID")?;
        let wrapping_private = B64
            .decode(required("SKWAD_WRAPPING_PRIVATE_KEY")?)
            .map_err(|_| "invalid wrapping private key")?;
        let wrapping_public = B64
            .decode(required("SKWAD_WRAPPING_PUBLIC_KEY")?)
            .map_err(|_| "invalid wrapping public key")?;
        Ok(Self {
            auth_token,
            signing: SigningKeyPair::from_bytes(signing_key_id, signing_secret),
            wrapping: DeviceKeyPair::from_bytes(wrapping_key_id, wrapping_private, wrapping_public)
                .map_err(|e| e.to_string())?,
        })
    }
}

fn required(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("{name} is required"))
}

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N], String> {
    B64.decode(value)
        .map_err(|_| "invalid base64 secret".to_string())?
        .try_into()
        .map_err(|_| format!("secret must be {N} bytes"))
}

fn print_generated_keys() {
    let signing = SigningKeyPair::generate("local-signing-v1");
    let wrapping = generate_device_keypair("local-wrapping-v1");
    let auth = generate_device_keypair("auth-randomness");
    println!("SKWAD_BACKEND_AUTH_TOKEN={}", B64.encode(auth.private_key_bytes()));
    println!("SKWAD_SIGNING_KEY_ID={}", signing.key_id);
    println!("SKWAD_SIGNING_PRIVATE_KEY={}", B64.encode(signing.secret_bytes()));
    println!("SKWAD_SIGNING_PUBLIC_KEY={}", B64.encode(signing.verifying_key_bytes()));
    println!("SKWAD_WRAPPING_KEY_ID={}", wrapping.key_id);
    println!(
        "SKWAD_WRAPPING_PRIVATE_KEY={}",
        B64.encode(wrapping.private_key_bytes())
    );
    println!("SKWAD_WRAPPING_PUBLIC_KEY={}", B64.encode(wrapping.public_key_bytes()));
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use skwad_catalogue::{
        build_payload, open_package, prepare_package, Argon2Parameters, CatalogueManifest, OpenCredential,
        PackageLimits, PublishOptions,
    };
    use tower::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn signing_endpoint_validates_then_signs_an_encrypted_catalogue() {
        let signing = SigningKeyPair::generate("test-signing");
        let verifying = signing.verifying_key_bytes();
        let wrapping = generate_device_keypair("test-wrapping");
        let state = Arc::new(BackendState {
            auth_token: "a-development-token-with-24-chars".into(),
            signing,
            wrapping,
        });

        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA user_version=1; CREATE TABLE package_info(schema_version INTEGER,library_id TEXT,shoot_id TEXT,published_revision INTEGER); CREATE TABLE shoot(id INTEGER,stable_id TEXT,library_id TEXT,name TEXT,status TEXT,notes TEXT,created_at TEXT,updated_at TEXT); CREATE TABLE people(id INTEGER); CREATE TABLE media(id INTEGER); CREATE TABLE clusters(id INTEGER); CREATE TABLE faces(id INTEGER); CREATE TABLE video_detections(id INTEGER); CREATE TABLE video_sample_frames(id INTEGER); CREATE TABLE albums(id INTEGER); CREATE TABLE album_media(id INTEGER); CREATE TABLE groups(id INTEGER); CREATE TABLE group_media(id INTEGER);").unwrap();
        let catalogue = connection.serialize("main").unwrap().to_vec();
        let payload = build_payload(
            CatalogueManifest {
                schema_version: 1,
                library_id: "lib".into(),
                shoot_id: "shoot".into(),
                published_revision: 1,
                created_at: "now".into(),
                catalogue_blake3: String::new(),
                media_count: 0,
            },
            &catalogue,
        )
        .unwrap();
        let unsigned = prepare_package(
            &payload,
            PublishOptions {
                package_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                recipients: &[Recipient {
                    key_id: state.wrapping.key_id.clone(),
                    public_key: state.wrapping.public_key_bytes().to_vec(),
                }],
                passphrase: Some("long test passphrase"),
                passphrase_key_id: "offline",
                argon2: Argon2Parameters {
                    memory_kib: 65_536,
                    iterations: 1,
                    parallelism: 1,
                },
            },
            "test-signing",
        )
        .unwrap();
        let response = app(state)
            .oneshot(
                Request::post("/v1/packages/sign")
                    .header(header::AUTHORIZATION, "Bearer a-development-token-with-24-chars")
                    .body(Body::from(unsigned))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let signed = to_bytes(response.into_body(), MAX_PACKAGE_BYTES).await.unwrap();
        assert!(open_package(
            &signed,
            OpenCredential::Passphrase("long test passphrase"),
            &verifying,
            PackageLimits::default()
        )
        .is_ok());
    }
}
