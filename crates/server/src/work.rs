//! `/api/work/*` and `/api/models*` — the front door for worker machines.
//!
//! A client that volunteers its GPU claims analysis jobs here, fetches the
//! bytes it cannot reach over a share, and posts back what it computed. The
//! server applies that result through the same `apply_analysis` its own
//! workers use, so a face found on a laptop is stored exactly as one found
//! here. Nothing in this module trusts the worker beyond its lease: every
//! job call is checked against the lease token the claim issued, and a
//! result that arrives after the lease lapsed is refused.
//!
//! Workers authenticate with a machine token, not a person's session (see
//! `skwad_app_core::work_api`), which [`require_machine`] resolves to the
//! enrolled [`Machine`].

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use skwad_app_core::analysis::apply_analysis;
use skwad_app_core::events;
use skwad_app_core::models::{ModelRegistry, ModelRole, ModelStatus};
use skwad_app_core::work_api::*;
use skwad_app_core::AppState;
use skwad_database::models::{Job, JobState, Media, ProcessingStatus};
use skwad_database::repo::jobs::{self, WorkerLane};
use skwad_database::repo::machines::{self, Machine};
use skwad_database::repo::{logs, media as media_repo, shoots, telemetry};
use skwad_database::Db;
use tower::ServiceExt;

use crate::error::{blocking, ApiError, ApiResult};
use crate::state::ServerState;

// --- authentication ---------------------------------------------------------------

/// Resolves the machine token a request presents and attaches the
/// [`Machine`]. Rejects everything else: these routes have no anonymous or
/// session-based form.
pub async fn require_machine(State(state): State<Arc<ServerState>>, mut request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(MACHINE_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| {
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|t| t.trim().to_string())
        });
    let Some(token) = token else {
        return ApiError::unauthorized("this route is for enrolled worker machines; send the machine token").into_response();
    };
    let core = Arc::clone(&state.core);
    let machine = blocking(move || {
        let mut conn = core.db.conn()?;
        Ok(machines::authenticate(&mut conn, &token)?)
    })
    .await;
    match machine {
        Ok(Some(machine)) => {
            request.extensions_mut().insert(machine);
            next.run(request).await
        }
        Ok(None) => ApiError::unauthorized("this machine is not enrolled, or its enrolment was revoked").into_response(),
        Err(error) => error.into_response(),
    }
}

// --- helpers -----------------------------------------------------------------------

fn server_models(core: &AppState) -> ModelStatus {
    let settings = core.settings();
    ModelRegistry::new(&core.paths.models).status(settings.detector_model.as_deref(), settings.embedder_model.as_deref())
}

fn lease_header(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get(LEASE_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request(format!("send the job's lease token in {LEASE_TOKEN_HEADER}")))
}

/// The job, if `machine` holds it under `token` right now. The database
/// gates on `complete`/`fail`/`release` check the token again; this is for
/// the calls that read or write around a job rather than settle it.
fn held_by(conn: &mut dyn Db, job_id: i64, machine: &Machine, token: &str) -> ApiResult<Job> {
    let job = jobs::get_by_id(conn, job_id)?.ok_or_else(|| ApiError::not_found("no such job"))?;
    let held = job.state == JobState::Running.as_str()
        && job.owner.as_deref() == Some(machine.id.as_str())
        && job.lease_token.as_deref() == Some(token);
    if !held {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "this machine no longer holds the lease on that job",
        ));
    }
    Ok(job)
}

fn media_of(conn: &mut dyn Db, job: &Job) -> ApiResult<Media> {
    job.media_id
        .and_then(|id| media_repo::get_by_id(conn, id).transpose())
        .transpose()?
        .ok_or_else(|| ApiError::not_found("the job's media is no longer indexed"))
}

fn settle_telemetry(conn: &mut dyn Db, job: &Job, succeeded: bool) {
    if let Err(error) = telemetry::mark_stage_settled(conn, job.shoot_id, &job.kind, succeeded) {
        tracing::warn!(shoot = job.shoot_id, %error, "could not finish stage telemetry");
    }
    if let Err(error) = telemetry::finalize_if_settled(conn, job.shoot_id) {
        tracing::warn!(shoot = job.shoot_id, %error, "could not finish processing telemetry");
    }
}

// --- settings and models -------------------------------------------------------------

/// `GET /api/work/settings` — the library-wide half of the settings plus
/// the model pair a worker has to match.
pub async fn settings(State(state): State<Arc<ServerState>>, Extension(_machine): Extension<Machine>) -> ApiResult<Json<SettingsResponse>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let status = server_models(&core);
        Ok(Json(SettingsResponse {
            library_version: core.library_version(),
            settings: core.settings().library(),
            detector_hash: status.detector_hash,
            embedder_hash: status.embedder_hash,
        }))
    })
    .await
}

/// `GET /api/models` — the detector and embedder this server analyses with.
pub async fn models(State(state): State<Arc<ServerState>>, Extension(_machine): Extension<Machine>) -> ApiResult<Json<ModelsResponse>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let settings = core.settings();
        let registry = ModelRegistry::new(&core.paths.models);
        let models = [
            registry.resolve_info(ModelRole::Detector, settings.detector_model.as_deref()),
            registry.resolve_info(ModelRole::Embedder, settings.embedder_model.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(Json(ModelsResponse { models }))
    })
    .await
}

/// `GET /api/models/{hash}` — the model file itself, with range support so
/// an interrupted download resumes.
pub async fn model_file(
    State(state): State<Arc<ServerState>>,
    Extension(_machine): Extension<Machine>,
    Path(hash): Path<String>,
    request: Request,
) -> ApiResult<Response> {
    let core = Arc::clone(&state.core);
    let path = blocking(move || {
        let registry = ModelRegistry::new(&core.paths.models);
        registry
            .find_by_hash(&hash)
            .map(|info| PathBuf::from(info.path))
            .ok_or_else(|| ApiError::not_found("no installed model has that hash"))
    })
    .await?;
    serve_file(path, request).await
}

async fn serve_file(path: PathBuf, request: Request) -> ApiResult<Response> {
    if !path.is_file() {
        return Err(ApiError::not_found("the file is not on the server"));
    }
    let response = match tower_http::services::ServeFile::new(path).oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    Ok(response.into_response())
}

// --- the queue ----------------------------------------------------------------------

/// `POST /api/work/claim` — the next analysis job for this machine, or
/// nothing. Refused while the worker's models differ from the server's:
/// vectors from a different embedder would poison recognition.
pub async fn claim(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Json(request): Json<ClaimRequest>,
) -> ApiResult<Json<ClaimResponse>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let status = server_models(&core);
        if !status.ready {
            return Err(ApiError::new(StatusCode::CONFLICT, "the server has no face models installed"));
        }
        let matches = request.capabilities.detector_hash == status.detector_hash
            && request.capabilities.embedder_hash == status.embedder_hash;
        if !matches {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "this machine's face models differ from the server's; sync models before claiming work",
            ));
        }

        let mut conn = core.db.conn()?;
        machines::touch(&mut conn, &machine.id, &request.capabilities)?;
        let library_version = core.library_version();
        let Some(job) = core.claim_job_as(&mut conn, WorkerLane::Remote, &machine.id)? else {
            return Ok(Json(ClaimResponse {
                job: None,
                library_version,
            }));
        };
        if let Err(error) = telemetry::mark_stage_started(&mut conn, job.shoot_id, &job.kind) {
            tracing::warn!(shoot = job.shoot_id, %error, "could not start processing telemetry");
        }
        let media = match media_of(&mut conn, &job) {
            Ok(media) => media,
            Err(_) => {
                // Nothing to ship: fail the row here rather than hand a
                // worker a job it can only fail back.
                let _ = jobs::fail(&mut conn, job.id, job.token().unwrap_or_default(), "media no longer indexed");
                return Ok(Json(ClaimResponse {
                    job: None,
                    library_version,
                }));
            }
        };
        let client_path = shoots::get_by_id(&mut conn, job.shoot_id)?
            .as_ref()
            .and_then(|shoot| skwad_database::paths::client_path(shoot, &media));
        tracing::debug!(job = job.id, machine = %machine.name, file = %media.filename, "job handed to a remote worker");
        Ok(Json(ClaimResponse {
            job: Some(ClaimedJob { job, media, client_path }),
            library_version,
        }))
    })
    .await
}

/// `POST /api/work/{id}/heartbeat` — keeps the lease alive and tells the
/// worker when to stop.
pub async fn heartbeat(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Path(job_id): Path<i64>,
    Json(request): Json<HeartbeatRequest>,
) -> ApiResult<Json<HeartbeatResponse>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let mut conn = core.db.conn()?;
        let library_version = core.library_version();
        let status = if held_by(&mut conn, job_id, &machine, &request.token).is_err() {
            HeartbeatStatus::LeaseLost
        } else {
            match jobs::heartbeat(&mut conn, job_id, &request.token)? {
                jobs::Heartbeat::Alive { .. } => HeartbeatStatus::Alive,
                jobs::Heartbeat::Cancelled => HeartbeatStatus::Cancelled,
                jobs::Heartbeat::LeaseLost => HeartbeatStatus::LeaseLost,
            }
        };
        Ok(Json(HeartbeatResponse { status, library_version }))
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct ArtifactQuery {
    /// Seconds into the video the frame is from.
    pub t: f64,
}

/// `POST /api/work/{id}/artifact?t=` — one review frame, as JPEG bytes.
pub async fn artifact(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Path(job_id): Path<i64>,
    Query(query): Query<ArtifactQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    if !query.t.is_finite() || query.t < 0.0 {
        return Err(ApiError::bad_request("t must be a non-negative number of seconds"));
    }
    if body.is_empty() {
        return Err(ApiError::bad_request("the frame is empty"));
    }
    let token = lease_header(&headers)?;
    let core = Arc::clone(&state.core);
    blocking(move || {
        let mut conn = core.db.conn()?;
        let job = held_by(&mut conn, job_id, &machine, &token)?;
        let media = media_of(&mut conn, &job)?;
        drop(conn);
        core.video_frames
            .store_encoded(&body, &media.content_key, query.t)
            .map_err(|error| ApiError::internal(format!("could not keep the review frame: {error}")))?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// `POST /api/work/{id}/result` — applies the analysis and settles the job,
/// in that order, under the lease.
pub async fn result(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Path(job_id): Path<i64>,
    Json(request): Json<ResultRequest>,
) -> ApiResult<Json<SettleResponse>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let mut conn = core.db.conn()?;
        let job = held_by(&mut conn, job_id, &machine, &request.token)?;
        let media = media_of(&mut conn, &job)?;
        drop(conn);

        let outcome = apply_analysis(&core.db, &core.video_frames, &media, request.output)
            .map_err(|error| ApiError::internal(format!("could not store the analysis: {error}")))?;

        let mut conn = core.db.conn()?;
        let settled = jobs::complete(&mut conn, job.id, &request.token)?;
        if settled {
            settle_telemetry(&mut conn, &job, true);
            tracing::info!(
                job = job.id,
                machine = %machine.name,
                file = %media.filename,
                faces = outcome.faces_detected,
                "analysis received from a remote worker"
            );
        } else {
            tracing::warn!(job = job.id, machine = %machine.name, "result arrived after the lease lapsed");
        }
        Ok(Json(SettleResponse { settled }))
    })
    .await
}

/// `POST /api/work/{id}/fail` — what a local worker's `finish_job` does on
/// failure, done here on the worker's behalf.
pub async fn fail(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Path(job_id): Path<i64>,
    Json(request): Json<FailRequest>,
) -> ApiResult<Json<FailResponse>> {
    let core = Arc::clone(&state.core);
    let sink = Arc::clone(&state.sink);
    blocking(move || {
        let mut conn = core.db.conn()?;
        let job = held_by(&mut conn, job_id, &machine, &request.token)?;
        let error = format!("{}: {}", machine.name, request.error);
        let state_after = jobs::fail(&mut conn, job.id, &request.token, &error)?;
        tracing::warn!(job = job.id, machine = %machine.name, error = %request.error, "remote job failed");
        if state_after == Some(JobState::Failed) {
            if let Some(media_id) = job.media_id {
                let _ = media_repo::set_status(&mut conn, media_id, ProcessingStatus::Failed, Some(&error));
            }
            let file = media_of(&mut conn, &job).ok().map(|m| m.filename);
            logs::record_quiet(
                &mut conn,
                logs::EVENT_PROCESSING_ERROR,
                Some(job.shoot_id),
                job.media_id,
                None,
                Some(&error),
            );
            events::emit(
                sink.as_ref(),
                events::JOB_FAILED,
                events::JobFailed {
                    shoot_id: job.shoot_id,
                    kind: job.kind.clone(),
                    file,
                    error,
                },
            );
            settle_telemetry(&mut conn, &job, false);
        }
        Ok(Json(FailResponse { state: state_after }))
    })
    .await
}

/// `POST /api/work/{id}/release` — hands the job back untouched, recording
/// why against the shoot when the worker was blocked.
pub async fn release(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Path(job_id): Path<i64>,
    Json(request): Json<ReleaseRequest>,
) -> ApiResult<Json<SettleResponse>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let mut conn = core.db.conn()?;
        let job = held_by(&mut conn, job_id, &machine, &request.token)?;
        let settled = jobs::release(&mut conn, job.id, &request.token)?;
        if let Some(reason) = request.blocked.as_deref() {
            core.record_blockage_for(Some(&machine.name), job.shoot_id, &job.kind, reason);
        }
        Ok(Json(SettleResponse { settled }))
    })
    .await
}

/// `GET /api/work/{id}/original` — the file a job refers to, for a worker
/// that cannot reach it over a share. Range requests are honoured.
pub async fn original(
    State(state): State<Arc<ServerState>>,
    Extension(machine): Extension<Machine>,
    Path(job_id): Path<i64>,
    request: Request,
) -> ApiResult<Response> {
    let token = lease_header(request.headers())?;
    let core = Arc::clone(&state.core);
    let path = blocking(move || {
        let mut conn = core.db.conn()?;
        let job = held_by(&mut conn, job_id, &machine, &token)?;
        let media = media_of(&mut conn, &job)?;
        Ok(PathBuf::from(media.path))
    })
    .await?;
    serve_file(path, request).await
}
