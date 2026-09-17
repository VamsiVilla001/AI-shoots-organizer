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

mod config;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod service;

fn main() {
    let subcommand = env::args().nth(1);
    match subcommand.as_deref() {
        Some("keygen") => {
            init_console_logging();
            print_generated_keys();
        }
        Some("help" | "--help" | "-h") => {
            init_console_logging();
            print_usage();
        }

        // Service management. Windows-only: elsewhere this is a foreground
        // process and the init system owns its lifecycle, so there is nothing
        // for the binary itself to install.
        #[cfg(windows)]
        Some(command @ ("install" | "uninstall" | "start" | "status")) => {
            init_console_logging();
            if let Err(error) = install::dispatch(command) {
                eprintln!("\n{error}");
                std::process::exit(1);
            }
        }
        #[cfg(not(windows))]
        Some(command @ ("install" | "uninstall" | "start" | "status")) => {
            eprintln!("`{command}` is Windows-only; use systemd or launchd to supervise this binary.");
            std::process::exit(2);
        }

        Some(other) => {
            eprintln!("unrecognised command: {other}\n");
            print_usage();
            std::process::exit(2);
        }

        // No subcommand: either the SCM started us, or a person did.
        None => run_foreground_or_service(),
    }
}

fn run_foreground_or_service() {
    // Ask the Service Control Manager to dispatch. It declines with 1063 when
    // nothing started us as a service, which is how "run from a shell" is
    // distinguished without a flag the SCM would have to be told to pass.
    #[cfg(windows)]
    {
        match service::try_run_as_service() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                init_console_logging();
                eprintln!("could not talk to the service control manager: {error}");
                std::process::exit(1);
            }
        }
    }

    init_console_logging();
    let result = serve_blocking(|| {}, async {
        let _ = tokio::signal::ctrl_c().await;
    });
    if let Err(error) = result {
        eprintln!("SKWAD backend: {error}");
        std::process::exit(2);
    }
}

fn print_usage() {
    println!(
        "skwad-backend — signs and rewraps SKWAD catalogue packages\n\n\
         USAGE\n  \
           skwad-backend              run in the foreground (Ctrl-C to stop)\n  \
           skwad-backend keygen       print a fresh set of secrets\n"
    );
    #[cfg(windows)]
    println!(
        "  skwad-backend install      register the Windows service (needs an elevated shell)\n  \
           skwad-backend start        start the installed service\n  \
           skwad-backend status       report whether it is installed and running\n  \
           skwad-backend uninstall    stop and deregister it\n"
    );
    println!(
        "CONFIGURATION\n  \
           Secrets come from the environment, then from {}.\n  \
           SKWAD_BACKEND_BIND sets the listen address (default 127.0.0.1:8787).",
        config::default_config_path().display()
    );
}

fn init_console_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init();
}

/// Runs the server on its own runtime until `shutdown` completes.
///
/// Split out of `main` because a Windows service entry point is a plain
/// function — it cannot be `#[tokio::main]`, and it has to own the runtime so
/// it can report `Stopped` to the SCM after the runtime winds down.
///
/// `on_listening` fires once the socket is accepting, which is the moment the
/// service is genuinely usable and therefore the moment to report `Running`.
fn serve_blocking(
    on_listening: impl FnOnce(),
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| format!("could not start the runtime: {error}"))?;
    runtime.block_on(async move {
        let state = Arc::new(BackendState::load()?);
        let bind = env::var("SKWAD_BACKEND_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .map_err(|error| format!("could not bind {bind}: {error}"))?;
        tracing::info!(%bind, "SKWAD backend listening");
        on_listening();
        axum::serve(listener, app(state))
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|error| format!("server stopped: {error}"))
    })
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
    /// Reads the secrets from the environment, then from the config file.
    ///
    /// Both sources matter: a developer exports the variables, while the
    /// service is started by the SCM with an empty environment block and can
    /// only get them from the file. See [`config`].
    fn load() -> Result<Self, String> {
        let source = config::Source::discover();
        let required = |name: &str| -> Result<String, String> {
            source.get(name).ok_or_else(|| {
                let file = source.path().display();
                if source.file_was_read() {
                    format!("{name} is not set, and {file} does not define it")
                } else {
                    format!(
                        "{name} is not set, and there is no config file at {file}.\n\
                         Run `skwad-backend install` to generate one, or `skwad-backend keygen` \
                         and export the values yourself."
                    )
                }
            })
        };

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

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N], String> {
    B64.decode(value)
        .map_err(|_| "invalid base64 secret".to_string())?
        .try_into()
        .map_err(|_| format!("secret must be {N} bytes"))
}

/// A fresh set of secrets, in the order they are written to a config file.
///
/// `SKWAD_SIGNING_PUBLIC_KEY` is included even though the service derives it
/// from the private key rather than reading it: it is the value clients need in
/// order to trust catalogues this backend signs, and having it recorded beside
/// the key it belongs to is what makes a key rotation auditable.
fn generate_secrets() -> Vec<(String, String)> {
    let signing = SigningKeyPair::generate("local-signing-v1");
    let wrapping = generate_device_keypair("local-wrapping-v1");
    let auth = generate_device_keypair("auth-randomness");
    vec![
        ("SKWAD_BACKEND_AUTH_TOKEN".into(), B64.encode(auth.private_key_bytes())),
        ("SKWAD_SIGNING_KEY_ID".into(), signing.key_id.clone()),
        ("SKWAD_SIGNING_PRIVATE_KEY".into(), B64.encode(signing.secret_bytes())),
        (
            "SKWAD_SIGNING_PUBLIC_KEY".into(),
            B64.encode(signing.verifying_key_bytes()),
        ),
        ("SKWAD_WRAPPING_KEY_ID".into(), wrapping.key_id.clone()),
        (
            "SKWAD_WRAPPING_PRIVATE_KEY".into(),
            B64.encode(wrapping.private_key_bytes()),
        ),
        (
            "SKWAD_WRAPPING_PUBLIC_KEY".into(),
            B64.encode(wrapping.public_key_bytes()),
        ),
    ]
}

fn print_generated_keys() {
    for (key, value) in generate_secrets() {
        println!("{key}={value}");
    }
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
