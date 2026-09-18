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
            // The tests drive the queue themselves; a local AI slot would
            // race the remote worker for the same jobs.
            local_analysis: false,
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

// --- worker machines -------------------------------------------------------------

fn machine_request(method: &str, uri: &str, machine_token: &str, lease: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-skwad-api", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .header(skwad_app_core::work_api::MACHINE_TOKEN_HEADER, machine_token);
    if let Some(lease) = lease {
        builder = builder.header(skwad_app_core::work_api::LEASE_TOKEN_HEADER, lease);
    }
    builder.body(body).unwrap()
}

/// The database client is synchronous; anything that touches it from a test
/// runs on a plain thread, as the harness itself does.
fn on_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().unwrap()
}

#[tokio::test]
async fn a_worker_machine_claims_computes_and_delivers_through_the_work_routes() {
    use skwad_database::models::{JobKind, MediaType, NewMedia, ProcessingStatus};
    use skwad_database::repo::{faces, jobs, media as media_repo, shoots};

    let h = harness();
    let auth_path = h.state().auth_dir.join("credentials.json");
    for (email, role) in [
        ("admin@example.com", skwad_app_core::api::UserRole::Admin),
        ("editor@example.com", skwad_app_core::api::UserRole::Member),
    ] {
        skwad_app_core::api::catalogue::upsert_user(&auth_path, email, "Someone", "correct horse battery", role).unwrap();
    }
    let sign_in = |email: &'static str| {
        let app = h.app.clone();
        async move {
            let response = app
                .oneshot(invoke(
                    "sign_in_skwad",
                    serde_json::json!({ "email": email, "password": "correct horse battery" }),
                    None,
                ))
                .await
                .unwrap();
            response.headers().get(auth::SESSION_HEADER).unwrap().to_str().unwrap().to_string()
        }
    };
    let admin = sign_in("admin@example.com").await;
    let editor = sign_in("editor@example.com").await;

    // The server's model pair: content, not names, is what a worker matches.
    let models_dir = h.state().core.paths.models.clone();
    std::fs::write(models_dir.join("scrfd_test.onnx"), b"detector bytes").unwrap();
    std::fs::write(models_dir.join("arcface_test.onnx"), b"embedder bytes").unwrap();

    // Enrolment is an administrator act.
    let refused = h
        .app
        .clone()
        .oneshot(invoke("enrol_machine", serde_json::json!({ "name": "Laptop", "machineId": "laptop-1" }), Some(&editor)))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let enrolled = h
        .app
        .clone()
        .oneshot(invoke("enrol_machine", serde_json::json!({ "name": "Laptop", "machineId": "laptop-1" }), Some(&admin)))
        .await
        .unwrap();
    assert_eq!(enrolled.status(), StatusCode::OK);
    let enrolled = body_json(enrolled).await;
    let machine_token = enrolled["token"].as_str().unwrap().to_string();
    assert_eq!(enrolled["machine"]["id"], "laptop-1");

    // Worker routes take the machine token and nothing else.
    let anonymous = Request::builder()
        .uri("/api/work/settings")
        .header("x-skwad-api", "1")
        .header(header::AUTHORIZATION, format!("Bearer {admin}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.app.clone().oneshot(anonymous).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let settings = h
        .app
        .clone()
        .oneshot(machine_request("GET", "/api/work/settings", &machine_token, None, Body::empty()))
        .await
        .unwrap();
    assert_eq!(settings.status(), StatusCode::OK);
    let settings = body_json(settings).await;
    let detector_hash = settings["detectorHash"].as_str().unwrap().to_string();
    let embedder_hash = settings["embedderHash"].as_str().unwrap().to_string();
    assert!(settings["settings"].is_object());

    let models = body_json(
        h.app
            .clone()
            .oneshot(machine_request("GET", "/api/models", &machine_token, None, Body::empty()))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(models["models"].as_array().unwrap().len(), 2);
    let model_file = h
        .app
        .clone()
        .oneshot(machine_request("GET", &format!("/api/models/{embedder_hash}"), &machine_token, None, Body::empty()))
        .await
        .unwrap();
    assert_eq!(model_file.status(), StatusCode::OK);
    assert_eq!(model_file.into_body().collect().await.unwrap().to_bytes().as_ref(), b"embedder bytes");

    // Three photos waiting for analysis, indexed and with real bytes.
    let source_dir = h.state().core.paths.root.join("shoot");
    std::fs::create_dir_all(&source_dir).unwrap();
    let core = Arc::clone(&h.state().core);
    let source = source_dir.clone();
    let (shoot_id, media_ids) = on_thread(move || {
        let mut conn = core.db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "Match day", &source.to_string_lossy()).unwrap();
        let mut ids = Vec::new();
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            let path = source.join(name);
            std::fs::write(&path, format!("bytes of {name}")).unwrap();
            let id = media_repo::upsert(
                &mut conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: path.to_string_lossy().into_owned(),
                    filename: name.into(),
                    media_type: MediaType::Photo,
                    extension: "jpg".into(),
                    file_size: 1,
                    content_key: format!("key-{name}"),
                    captured_at: None,
                    normalized_relative_path: None,
                },
            )
            .unwrap();
            media_repo::set_status(&mut conn, id, ProcessingStatus::Thumbnailed, None).unwrap();
            jobs::enqueue(&mut conn, shoot.id, JobKind::AnalysePhoto, Some(id), 300, None).unwrap();
            ids.push(id);
        }
        // A finishing stage the remote lane must never hand out.
        jobs::enqueue(&mut conn, shoot.id, JobKind::Albums, None, 500, None).unwrap();
        (shoot.id, ids)
    });

    // Wrong models: refused, so foreign vectors never reach the library.
    let mismatched = serde_json::json!({ "capabilities": {
        "appVersion": "test", "gpu": null, "detectorHash": detector_hash, "embedderHash": "different",
        "aiWorkers": 1, "onBattery": false } });
    let refused = h
        .app
        .clone()
        .oneshot(machine_request("POST", "/api/work/claim", &machine_token, None, Body::from(mismatched.to_string())))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    let capabilities = serde_json::json!({ "capabilities": {
        "appVersion": "test", "gpu": "DirectML", "detectorHash": detector_hash, "embedderHash": embedder_hash,
        "aiWorkers": 2, "onBattery": false } });
    let claim = || {
        let app = h.app.clone();
        let body = capabilities.to_string();
        let token = machine_token.clone();
        async move {
            let response = app
                .oneshot(machine_request("POST", "/api/work/claim", &token, None, Body::from(body)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            body_json(response).await
        }
    };

    // --- job one: computed and delivered ---
    let first = claim().await;
    let job = &first["job"];
    assert!(job.is_object(), "{first}");
    assert_eq!(job["job"]["kind"], "analysePhoto");
    assert_eq!(job["job"]["owner"], "laptop-1");
    assert_eq!(job["media"]["id"], media_ids[0]);
    assert!(job["clientPath"].is_null(), "no share mapping on this shoot");
    let job_id = job["job"]["id"].as_i64().unwrap();
    let lease = job["job"]["leaseToken"].as_str().unwrap().to_string();

    let beat = body_json(
        h.app
            .clone()
            .oneshot(machine_request(
                "POST",
                &format!("/api/work/{job_id}/heartbeat"),
                &machine_token,
                None,
                Body::from(serde_json::json!({ "token": lease }).to_string()),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(beat["status"], "alive");

    // The bytes, for a worker that cannot see the share.
    let original = h
        .app
        .clone()
        .oneshot(machine_request("GET", &format!("/api/work/{job_id}/original"), &machine_token, Some(&lease), Body::empty()))
        .await
        .unwrap();
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(original.into_body().collect().await.unwrap().to_bytes().as_ref(), b"bytes of a.jpg");
    let wrong_lease = h
        .app
        .clone()
        .oneshot(machine_request("GET", &format!("/api/work/{job_id}/original"), &machine_token, Some("nope"), Body::empty()))
        .await
        .unwrap();
    assert_eq!(wrong_lease.status(), StatusCode::CONFLICT);

    let frame = h
        .app
        .clone()
        .oneshot(machine_request(
            "POST",
            &format!("/api/work/{job_id}/artifact?t=1.5"),
            &machine_token,
            Some(&lease),
            Body::from(vec![0xFF, 0xD8, 0xFF, 0xD9]),
        ))
        .await
        .unwrap();
    assert_eq!(frame.status(), StatusCode::NO_CONTENT);
    assert!(h.state().core.video_frames.read("key-a.jpg", 1.5).unwrap().is_some());

    let output = serde_json::json!({
        "token": lease,
        "output": {
            "orientation": null,
            "faces": [{ "bbox": { "x": 0.1, "y": 0.1, "w": 0.2, "h": 0.3 }, "landmarks": null,
                        "detectionConfidence": 0.93, "embedding": null, "quality": 0.7, "frameTime": null }],
            "sampleTimes": [],
            "skipped": null,
            "embedderKey": embedder_hash,
            "framesAnalysed": 1
        }
    });
    let delivered = body_json(
        h.app
            .clone()
            .oneshot(machine_request("POST", &format!("/api/work/{job_id}/result"), &machine_token, None, Body::from(output.to_string())))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(delivered["settled"], true);
    let core = Arc::clone(&h.state().core);
    let media_a = media_ids[0];
    let (face_count, model_key, job_state, media_status) = on_thread(move || {
        let mut conn = core.db.conn().unwrap();
        let faces = faces::for_media(&mut conn, media_a).unwrap();
        let job = jobs::get_by_id(&mut conn, job_id).unwrap().unwrap();
        let media = media_repo::get_by_id(&mut conn, media_a).unwrap().unwrap();
        (faces.len(), faces[0].model_key.clone(), job.state, media.processing_status)
    });
    assert_eq!(face_count, 1);
    assert_eq!(model_key.as_deref(), Some(embedder_hash.as_str()));
    assert_eq!(job_state, "done");
    assert_eq!(media_status, "analysed");

    // A second delivery of the same job is refused: the lease is gone.
    let again = h
        .app
        .clone()
        .oneshot(machine_request("POST", &format!("/api/work/{job_id}/result"), &machine_token, None, Body::from(output.to_string())))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // --- job two: failed on the worker ---
    let second = claim().await;
    let job_id = second["job"]["job"]["id"].as_i64().unwrap();
    let lease = second["job"]["job"]["leaseToken"].as_str().unwrap().to_string();
    let failed = body_json(
        h.app
            .clone()
            .oneshot(machine_request(
                "POST",
                &format!("/api/work/{job_id}/fail"),
                &machine_token,
                None,
                Body::from(serde_json::json!({ "token": lease, "error": "decode failed" }).to_string()),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(failed["state"], "queued", "first failure goes back for a retry");

    // --- job three: blocked on the worker, handed back with the reason ---
    let third = claim().await;
    let job_id = third["job"]["job"]["id"].as_i64().unwrap();
    let lease = third["job"]["job"]["leaseToken"].as_str().unwrap().to_string();
    let released = body_json(
        h.app
            .clone()
            .oneshot(machine_request(
                "POST",
                &format!("/api/work/{job_id}/release"),
                &machine_token,
                None,
                Body::from(serde_json::json!({ "token": lease, "blocked": "FFmpeg is not installed" }).to_string()),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(released["settled"], true);
    let blockage = h.state().core.blockage(shoot_id).expect("recorded against the shoot");
    assert_eq!(blockage.machine.as_deref(), Some("Laptop"));
    assert_eq!(blockage.describe(), "Laptop: FFmpeg is not installed");

    // The roster shows the machine and what it did.
    let roster = body_json(
        h.app
            .clone()
            .oneshot(invoke("list_machines", serde_json::Value::Null, Some(&editor)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(roster[0]["id"], "laptop-1");
    assert_eq!(roster[0]["completed"], 1);
    assert_eq!(roster[0]["capabilities"]["gpu"], "DirectML");
    assert!(roster[0]["lastSeen"].is_string());

    // Revoked: the token stops working at the next call.
    let revoked = h
        .app
        .clone()
        .oneshot(invoke("revoke_machine", serde_json::json!({ "machineId": "laptop-1" }), Some(&admin)))
        .await
        .unwrap();
    assert_eq!(body_json(revoked).await, serde_json::json!(true));
    let after = h
        .app
        .clone()
        .oneshot(machine_request("POST", "/api/work/claim", &machine_token, None, Body::from(capabilities.to_string())))
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}
