//! The front door end to end: boot against a private test schema, sign in,
//! run a command, and check the gates — no session, no version header, no
//! admin role — answer the way a client can act on.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use skwad_app_core::AppPaths;
use skwad_database::Database;
use skwad_server::{auth, router, ServerConfig};
use tower::ServiceExt;

struct Harness {
    app: axum::Router,
    /// Taken by `Drop`, so the pool closes on a plain thread rather than
    /// when the field falls out of scope on the runtime thread.
    state: Option<Arc<skwad_server::ServerState>>,
    workers: Option<skwad_app_core::WorkerPool>,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Shutting the workers down and closing the pool both touch the
        // synchronous database client, which must not run on the test's
        // async runtime thread — same reason the boot happens off-thread.
        let workers = self.workers.take();
        let state = self.state.take().expect("dropped once");
        let app = std::mem::replace(&mut self.app, axum::Router::new());
        std::thread::spawn(move || {
            state.core.begin_shutdown();
            if let Some(workers) = workers {
                workers.join();
            }
            drop(app);
            drop(state);
        })
        .join()
        .unwrap();
    }
}

impl Harness {
    fn state(&self) -> &skwad_server::ServerState {
        self.state.as_ref().expect("still alive")
    }
}

fn harness() -> Harness {
    // The database client is synchronous and drives its own runtime, which
    // cannot be started from inside the test's async runtime — so the boot
    // happens on a plain thread, exactly as `main` does it.
    std::thread::spawn(|| {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        let config = ServerConfig {
            library_root: library.clone(),
            machine_settings_file: Some(dir.path().join("machine").join("machine-settings.json")),
            media_roots: vec![dir.path().to_path_buf()],
            ..Default::default()
        };
        let paths = AppPaths::create(&library).unwrap();
        let db = Database::open_test().unwrap();
        let (state, workers) = skwad_server::boot_with(config, paths, db).unwrap();
        let app = router(Arc::clone(&state));
        Harness {
            app,
            state: Some(state),
            workers: Some(workers),
            _dir: dir,
        }
    })
    .join()
    .unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string()))
    }
}

fn invoke(command: &str, args: serde_json::Value, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/invoke/{command}"))
        .header("x-skwad-api", "1")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(args.to_string())).unwrap()
}

#[tokio::test]
async fn health_answers_without_a_session_but_hides_details() {
    let h = harness();
    let response = h
        .app
        .clone()
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["apiVersion"], 1);
    let checks = json["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["name"] == "database" && c["ok"] == true));
    assert!(checks.iter().all(|c| c.get("detail").is_none()), "details are for signed-in callers");
}

#[tokio::test]
async fn every_api_call_must_name_its_version() {
    let h = harness();
    let no_header = Request::builder()
        .method("POST")
        .uri("/api/invoke/catalogue_session_status")
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(no_header).await.unwrap();
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);

    let wrong = Request::builder()
        .method("POST")
        .uri("/api/invoke/catalogue_session_status")
        .header("x-skwad-api", "99")
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(wrong).await.unwrap();
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let json = body_json(response).await;
    assert!(json["message"].as_str().unwrap().contains("version 99"));
}

#[tokio::test]
async fn sign_in_hands_out_a_session_that_gates_everything_else() {
    let h = harness();

    // Signed out: the public command works, the rest is refused.
    let status = h
        .app
        .clone()
        .oneshot(invoke("catalogue_session_status", serde_json::Value::Null, None))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(body_json(status).await["authenticatedOnce"], false);

    let refused = h
        .app
        .clone()
        .oneshot(invoke("list_shoots", serde_json::Value::Null, None))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

    // An account, created the way a headless box gets its first one.
    let auth_path = h.state().auth_dir.join("credentials.json");
    skwad_app_core::api::catalogue::upsert_user(
        &auth_path,
        "editor@example.com",
        "Editor",
        "correct horse battery",
        skwad_app_core::api::UserRole::Member,
    )
    .unwrap();

    let wrong = h
        .app
        .clone()
        .oneshot(invoke(
            "sign_in_skwad",
            serde_json::json!({ "email": "editor@example.com", "password": "nope" }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);

    let signed_in = h
        .app
        .clone()
        .oneshot(invoke(
            "sign_in_skwad",
            serde_json::json!({ "email": "editor@example.com", "password": "correct horse battery" }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(signed_in.status(), StatusCode::OK);
    let cookie = signed_in
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with(&format!("{}=", auth::COOKIE_NAME)), "{cookie}");
    let token = signed_in
        .headers()
        .get(auth::SESSION_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let json = body_json(signed_in).await;
    assert_eq!(json["authenticatedOnce"], true);
    assert_eq!(json["email"], "editor@example.com");

    // The token opens the door — as a bearer header…
    let shoots = h
        .app
        .clone()
        .oneshot(invoke("list_shoots", serde_json::Value::Null, Some(&token)))
        .await
        .unwrap();
    assert_eq!(shoots.status(), StatusCode::OK);
    assert_eq!(body_json(shoots).await, serde_json::json!([]));

    // …and as the cookie, which is what `<img>` and `EventSource` send.
    let with_cookie = Request::builder()
        .method("POST")
        .uri("/api/invoke/catalogue_session_status")
        .header("x-skwad-api", "1")
        .header(header::COOKIE, format!("{}={token}", auth::COOKIE_NAME))
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(with_cookie).await.unwrap();
    assert_eq!(body_json(response).await["authenticatedOnce"], true);

    // Bad arguments are the caller's problem, not a crash.
    let bad = h
        .app
        .clone()
        .oneshot(invoke("get_shoot", serde_json::json!({ "shootId": "seven" }), Some(&token)))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // A member cannot wipe everyone's embeddings.
    let forbidden = h
        .app
        .clone()
        .oneshot(invoke("clear_all_embeddings", serde_json::Value::Null, Some(&token)))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // Media needs the session too, and answers 404 for an unknown id rather
    // than leaking whether anything exists.
    let media = Request::builder()
        .uri("/media/thumb/1")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(media).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let anonymous = Request::builder().uri("/media/thumb/1").body(Body::empty()).unwrap();
    assert_eq!(h.app.clone().oneshot(anonymous).await.unwrap().status(), StatusCode::UNAUTHORIZED);

    // Signing out revokes the token.
    let out = h
        .app
        .clone()
        .oneshot(invoke("sign_out_skwad", serde_json::Value::Null, Some(&token)))
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::OK);
    let after = h
        .app
        .clone()
        .oneshot(invoke("list_shoots", serde_json::Value::Null, Some(&token)))
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_administrator_may_run_maintenance_and_it_is_audited() {
    let h = harness();
    let auth_path = h.state().auth_dir.join("credentials.json");
    skwad_app_core::api::catalogue::upsert_user(
        &auth_path,
        "admin@example.com",
        "Admin",
        "correct horse battery",
        skwad_app_core::api::UserRole::Admin,
    )
    .unwrap();
    let signed_in = h
        .app
        .clone()
        .oneshot(invoke(
            "sign_in_skwad",
            serde_json::json!({ "email": "admin@example.com", "password": "correct horse battery" }),
            None,
        ))
        .await
        .unwrap();
    let token = signed_in.headers().get(auth::SESSION_HEADER).unwrap().to_str().unwrap().to_string();

    let cleared = h
        .app
        .clone()
        .oneshot(invoke("clear_all_embeddings", serde_json::Value::Null, Some(&token)))
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);

    let logs = h
        .app
        .clone()
        .oneshot(invoke("recent_logs", serde_json::json!({ "shootId": null, "limit": 20 }), Some(&token)))
        .await
        .unwrap();
    let entries = body_json(logs).await;
    assert!(
        entries
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "admin_action" && e["detail"].as_str().unwrap().contains("admin@example.com ran clear_all_embeddings")),
        "{entries}"
    );
}
