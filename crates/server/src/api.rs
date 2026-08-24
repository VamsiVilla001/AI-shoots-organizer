//! The HTTP port of `commands.rs`, one route per command.
//!
//! Deliberately mechanical: same names, same inputs, same outputs, same order,
//! so the two front doors can be read side by side and compared. The
//! duplication is temporary — once the desktop shell talks to a loopback server
//! the Tauri handlers go away and this becomes the only copy.
//!
//! Every handler that touches the database does so inside [`blocking`], because
//! the core is synchronous and must never occupy an async runtime thread.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use teo_app_core::models::{ModelRegistry, ModelStatus};
use teo_app_core::settings::AppSettings;
use teo_app_core::{events, export, stages};
use teo_database::models::*;
use teo_database::repo::{
    albums, clusters, exports, faces, groups, jobs, logs, media as media_repo, people, shoots, video,
};
use teo_export_engine::ExportOptions;

use crate::config::resolve_new_within_roots;
use crate::error::{blocking, ApiError, ApiResult};
use crate::state::ServerState;

type Ctx = State<Arc<ServerState>>;

/// `{ "shootId": 3 }` — the body shape for the handful of commands whose only
/// argument is a shoot.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootRef {
    pub shoot_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootScope {
    pub shoot_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

/// Mirrors the desktop's `AppInfo` field for field, so the front end sees the
/// same JSON from either front door.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub version: String,
    pub paths: teo_app_core::AppPaths,
    pub media_url_base: String,
    pub ffmpeg_available: bool,
    pub ffmpeg_version: Option<String>,
    pub models: ModelStatus,
    pub accelerators: Vec<teo_face_detection::Accelerator>,
    pub cpu_cores: usize,
    pub supported_extensions: Vec<String>,
    pub cache_bytes: u64,
}

pub async fn system_status(State(state): Ctx) -> ApiResult<Json<SystemStatus>> {
    let core = Arc::clone(&state.core);
    let status = blocking(move || {
        let settings = core.settings();
        let ffmpeg = teo_app_core::pipeline::discover_ffmpeg(&settings);
        let registry = ModelRegistry::new(&core.paths.models);

        Ok(SystemStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            paths: core.paths.clone(),
            media_url_base: core.media_url_base.clone(),
            ffmpeg_available: ffmpeg.is_some(),
            ffmpeg_version: ffmpeg.as_ref().and_then(|f| f.version()),
            models: registry
                .status(settings.detector_model.as_deref(), settings.embedder_model.as_deref()),
            accelerators: teo_face_detection::available_accelerators(),
            cpu_cores: num_cpus::get(),
            supported_extensions: teo_media_core::formats::supported_extensions()
                .into_iter()
                .map(String::from)
                .collect(),
            cache_bytes: core.paths.cache_size(),
        })
    })
    .await?;

    Ok(Json(status))
}

pub async fn model_status(State(state): Ctx) -> ApiResult<Json<ModelStatus>> {
    let core = Arc::clone(&state.core);
    let status = blocking(move || {
        let settings = core.settings();
        Ok(ModelRegistry::new(&core.paths.models)
            .status(settings.detector_model.as_deref(), settings.embedder_model.as_deref()))
    })
    .await?;
    Ok(Json(status))
}

pub async fn get_settings(State(state): Ctx) -> Json<AppSettings> {
    Json(state.core.settings())
}

pub async fn update_settings(
    State(state): Ctx,
    Json(settings): Json<AppSettings>,
) -> ApiResult<Json<AppSettings>> {
    let core = Arc::clone(&state.core);
    let saved = blocking(move || Ok(core.update_settings(settings)?)).await?;
    Ok(Json(saved))
}

// ---------------------------------------------------------------------------
// Shoots
// ---------------------------------------------------------------------------

pub async fn list_shoots(State(state): Ctx) -> ApiResult<Json<Vec<ShootSummary>>> {
    let core = Arc::clone(&state.core);
    let summaries = blocking(move || {
        let conn = core.db.conn()?;
        Ok(shoots::list_summaries(&conn)?)
    })
    .await?;
    Ok(Json(summaries))
}

pub async fn get_shoot(State(state): Ctx, Path(shoot_id): Path<i64>) -> ApiResult<Json<ShootSummary>> {
    let core = Arc::clone(&state.core);
    let summary = blocking(move || {
        let conn = core.db.conn()?;
        shoots::summary(&conn, shoot_id)?.ok_or_else(|| ApiError::not_found("no such shoot"))
    })
    .await?;
    Ok(Json(summary))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateShoot {
    pub name: String,
    pub source_path: String,
}

/// Creates a shoot and immediately queues the scan.
///
/// Unlike the desktop, the source folder arrives as text rather than from a
/// native picker, so it is checked against the configured media roots before
/// anything is written.
pub async fn create_shoot(State(state): Ctx, Json(body): Json<CreateShoot>) -> ApiResult<Json<Shoot>> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("give the shoot a name"));
    }

    let roots = state.config.media_roots.clone();
    let core = Arc::clone(&state.core);
    let requested = PathBuf::from(&body.source_path);

    let shoot = blocking(move || {
        let source = if roots.is_empty() {
            requested.clone()
        } else {
            crate::config::resolve_within_roots(&requested, &roots)?
        };
        if !source.is_dir() {
            return Err(ApiError::bad_request(format!("{} is not a folder", source.display())));
        }

        let shoot = {
            let conn = core.db.conn()?;
            let shoot = shoots::create(&conn, &name, &source.display().to_string())?;
            jobs::enqueue(&conn, shoot.id, JobKind::Scan, None, stages::priority::SCAN, None)?;
            shoot
        };

        core.resume_shoot(shoot.id);
        events::shoot_changed(core.sink(), shoot.id, "created");
        Ok(shoot)
    })
    .await?;

    Ok(Json(shoot))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameBody {
    pub name: String,
}

pub async fn rename_shoot(
    State(state): Ctx,
    Path(shoot_id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("give the shoot a name"));
    }
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        shoots::rename(&conn, shoot_id, &name)?;
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Removes the shoot's index. The user's media is not touched (§21).
pub async fn delete_shoot_index(
    State(state): Ctx,
    Path(shoot_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        core.cancel_shoot(shoot_id);
        let conn = core.db.conn()?;
        jobs::cancel_for_shoot(&conn, shoot_id)?;
        shoots::delete_index(&conn, shoot_id)?;
        logs::record_quiet(&conn, logs::EVENT_SHOOT_DELETED, Some(shoot_id), None, None, None);
        events::shoot_changed(core.sink(), shoot_id, "deleted");
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn resume_processing(State(state): Ctx, Path(shoot_id): Path<i64>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let queued = blocking(move || {
        core.resume_shoot(shoot_id);
        core.set_paused(false);

        let conn = core.db.conn()?;
        jobs::retry_failed(&conn, shoot_id)?;
        jobs::enqueue_unique(&conn, shoot_id, JobKind::Scan, None, stages::priority::SCAN)?;
        drop(conn);

        let queued = stages::queue_pending_work(&core.db, shoot_id)?;
        events::shoot_changed(core.sink(), shoot_id, "resumed");
        Ok(queued)
    })
    .await?;
    Ok(Json(queued))
}

pub async fn cancel_processing(State(state): Ctx, Path(shoot_id): Path<i64>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let cancelled = blocking(move || {
        core.cancel_shoot(shoot_id);
        let conn = core.db.conn()?;
        let cancelled = jobs::cancel_for_shoot(&conn, shoot_id)?;
        shoots::set_status(&conn, shoot_id, ShootStatus::Paused)?;
        events::shoot_changed(core.sink(), shoot_id, "cancelled");
        Ok(cancelled)
    })
    .await?;
    Ok(Json(cancelled))
}

pub async fn reanalyse_shoot(State(state): Ctx, Path(shoot_id): Path<i64>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let queued = blocking(move || {
        stages::reset_analysis(&core.db, shoot_id)?;
        core.resume_shoot(shoot_id);
        let queued = stages::queue_pending_work(&core.db, shoot_id)?;
        events::shoot_changed(core.sink(), shoot_id, "reanalysing");
        Ok(queued)
    })
    .await?;
    Ok(Json(queued))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseBody {
    pub paused: bool,
}

/// Pausing is process-wide, exactly as the desktop command is — hence a route
/// under `/api/processing` rather than under one shoot.
pub async fn pause_processing(State(state): Ctx, Json(body): Json<PauseBody>) -> Json<bool> {
    state.core.set_paused(body.paused);
    Json(body.paused)
}

pub async fn get_progress(
    State(state): Ctx,
    Path(shoot_id): Path<i64>,
) -> ApiResult<Json<ProcessingProgress>> {
    let core = Arc::clone(&state.core);
    let progress = blocking(move || {
        let conn = core.db.conn()?;
        Ok(jobs::progress(&conn, shoot_id)?)
    })
    .await?;
    Ok(Json(progress))
}

pub async fn list_failed_jobs(State(state): Ctx, Path(shoot_id): Path<i64>) -> ApiResult<Json<Vec<Job>>> {
    let core = Arc::clone(&state.core);
    let failed = blocking(move || {
        let conn = core.db.conn()?;
        Ok(jobs::list_failed(&conn, shoot_id, 200)?)
    })
    .await?;
    Ok(Json(failed))
}

/// The queue at a glance across every shoot. There is no desktop command for
/// this — the sidebar reads per-shoot progress — but a browser with no shoot
/// open still needs to know whether the server is busy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobsSummary {
    pub shoots: usize,
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
    pub paused: bool,
}

pub async fn jobs_summary(State(state): Ctx) -> ApiResult<Json<JobsSummary>> {
    let core = Arc::clone(&state.core);
    let summary = blocking(move || {
        let conn = core.db.conn()?;
        let all = shoots::list_summaries(&conn)?;
        Ok(JobsSummary {
            shoots: all.len(),
            queued: all.iter().map(|s| s.pending_jobs).sum(),
            running: 0,
            failed: all.iter().map(|s| s.failed_jobs).sum(),
            paused: core.is_paused(),
        })
    })
    .await?;
    Ok(Json(summary))
}

/// Removes all scanned shoot indexes and generated thumbnails while keeping
/// settings, player profiles, logs and installed models.
pub async fn clear_scanned_data(State(state): Ctx) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let removed = blocking(move || {
        core.set_paused(true);

        let cleared = (|| -> ApiResult<usize> {
            let shoot_ids = {
                let conn = core.db.conn()?;
                shoots::list(&conn)?.into_iter().map(|shoot| shoot.id).collect::<Vec<_>>()
            };
            for shoot_id in shoot_ids {
                core.cancel_shoot(shoot_id);
            }
            Ok(core.db.transaction(shoots::clear_all_indexes)?)
        })();

        // Never leave processing paused if the database operation fails.
        core.set_paused(false);
        let removed = cleared?;

        match core.thumbnails.clear() {
            Ok(count) => tracing::info!(shoots = removed, thumbnails = count, "cleared scanned data"),
            Err(error) => {
                tracing::warn!(%error, "scan indexes were cleared but some thumbnails could not be removed")
            }
        }

        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(removed)
    })
    .await?;
    Ok(Json(removed))
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

pub async fn list_media(
    State(state): Ctx,
    Path(shoot_id): Path<i64>,
    Query(mut query): Query<MediaQuery>,
) -> ApiResult<Json<Vec<Media>>> {
    // The path is the authority on which shoot is being read.
    query.shoot_id = Some(shoot_id);
    let core = Arc::clone(&state.core);
    let media = blocking(move || {
        let conn = core.db.conn()?;
        Ok(media_repo::query(&conn, &query)?)
    })
    .await?;
    Ok(Json(media))
}

pub async fn get_media(State(state): Ctx, Path(media_id): Path<i64>) -> ApiResult<Json<Media>> {
    let core = Arc::clone(&state.core);
    let item = blocking(move || {
        let conn = core.db.conn()?;
        media_repo::get_by_id(&conn, media_id)?.ok_or_else(|| ApiError::not_found("not indexed"))
    })
    .await?;
    Ok(Json(item))
}

pub async fn media_faces(State(state): Ctx, Path(media_id): Path<i64>) -> ApiResult<Json<Vec<Face>>> {
    let core = Arc::clone(&state.core);
    let found = blocking(move || {
        let conn = core.db.conn()?;
        Ok(faces::for_media(&conn, media_id)?)
    })
    .await?;
    Ok(Json(found))
}

pub async fn video_timelines(
    State(state): Ctx,
    Path(media_id): Path<i64>,
) -> ApiResult<Json<Vec<VideoTimeline>>> {
    let core = Arc::clone(&state.core);
    let timelines = blocking(move || {
        let conn = core.db.conn()?;
        Ok(video::timelines(&conn, media_id)?)
    })
    .await?;
    Ok(Json(timelines))
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

pub async fn list_people(
    State(state): Ctx,
    Query(scope): Query<ShootScope>,
) -> ApiResult<Json<Vec<PersonSummary>>> {
    let core = Arc::clone(&state.core);
    let people_list = blocking(move || {
        let conn = core.db.conn()?;
        Ok(people::list_summaries(&conn, scope.shoot_id)?)
    })
    .await?;
    Ok(Json(people_list))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePerson {
    pub name: String,
    pub team: Option<String>,
}

pub async fn create_person(State(state): Ctx, Json(body): Json<CreatePerson>) -> ApiResult<Json<Person>> {
    let core = Arc::clone(&state.core);
    let person = blocking(move || {
        let conn = core.db.conn()?;
        let person = people::get_or_create(&conn, &body.name, body.team.as_deref())?;
        logs::record_quiet(
            &conn,
            logs::EVENT_PLAYER_CREATED,
            None,
            None,
            Some(person.id),
            Some(&person.name),
        );
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(person)
    })
    .await?;
    Ok(Json(person))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePerson {
    pub name: Option<String>,
    pub team: Option<String>,
    pub notes: Option<String>,
}

/// Covers both `rename_person` and `update_person`: the desktop splits them
/// because Tauri commands take fixed arguments, but one PATCH is the same two
/// repository calls.
pub async fn update_person(
    State(state): Ctx,
    Path(person_id): Path<i64>,
    Json(body): Json<UpdatePerson>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        if let Some(name) = body.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            people::rename(&conn, person_id, name)?;
            logs::record_quiet(
                &conn,
                logs::EVENT_PLAYER_RENAMED,
                None,
                None,
                Some(person_id),
                Some(name),
            );
        }
        people::update(&conn, person_id, body.team.as_deref(), body.notes.as_deref())?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeBody {
    pub source_id: i64,
}

pub async fn merge_people(
    State(state): Ctx,
    Path(target_id): Path<i64>,
    Json(body): Json<MergeBody>,
) -> ApiResult<Json<i64>> {
    let core = Arc::clone(&state.core);
    let moved = blocking(move || {
        let moved = core.db.transaction(|conn| people::merge(conn, target_id, body.source_id))?;
        let conn = core.db.conn()?;
        logs::record_quiet(
            &conn,
            logs::EVENT_PLAYER_MERGED,
            None,
            None,
            Some(target_id),
            Some(&format!("absorbed player {}; {moved} faces now on the target", body.source_id)),
        );
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(moved)
    })
    .await?;
    Ok(Json(moved))
}

pub async fn delete_person(
    State(state): Ctx,
    Path(person_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        people::delete(&conn, person_id)?;
        logs::record_quiet(&conn, logs::EVENT_PLAYER_DELETED, None, None, Some(person_id), None);
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Drops a player's biometric data but keeps the profile (§22, §24).
pub async fn clear_person_recognition(
    State(state): Ctx,
    Path(person_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        people::clear_recognition_data(&conn, person_id)?;
        logs::record_quiet(
            &conn,
            logs::EVENT_RECOGNITION_DATA_CLEARED,
            None,
            None,
            Some(person_id),
            None,
        );
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Clusters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterQuery {
    pub shoot_id: i64,
    #[serde(default)]
    pub include_named: bool,
}

pub async fn list_clusters(
    State(state): Ctx,
    Query(query): Query<ClusterQuery>,
) -> ApiResult<Json<Vec<ClusterSummary>>> {
    let core = Arc::clone(&state.core);
    let found = blocking(move || {
        let conn = core.db.conn()?;
        Ok(clusters::list_summaries(&conn, query.shoot_id, query.include_named)?)
    })
    .await?;
    Ok(Json(found))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameCluster {
    pub name: String,
    pub team: Option<String>,
}

/// Names an unknown cluster, promoting it to a player and adding every face in
/// it to the library (§7).
pub async fn name_cluster(
    State(state): Ctx,
    Path(cluster_id): Path<i64>,
    Json(body): Json<NameCluster>,
) -> ApiResult<Json<Person>> {
    let core = Arc::clone(&state.core);
    let person = blocking(move || {
        let person = core.db.transaction(|conn| {
            let person = people::get_or_create(conn, &body.name, body.team.as_deref())?;
            let faces_named = clusters::name_cluster(conn, cluster_id, person.id)?;
            logs::record_quiet(
                conn,
                logs::EVENT_CLUSTER_NAMED,
                None,
                None,
                Some(person.id),
                Some(&format!("cluster {cluster_id} named with {faces_named} faces")),
            );
            Ok(person)
        })?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(person)
    })
    .await?;
    Ok(Json(person))
}

pub async fn merge_clusters(
    State(state): Ctx,
    Path(target_id): Path<i64>,
    Json(body): Json<MergeBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        core.db.transaction(|conn| {
            clusters::merge(conn, target_id, body.source_id)?;
            logs::record_quiet(
                conn,
                logs::EVENT_CLUSTER_MERGED,
                None,
                None,
                None,
                Some(&format!("cluster {} merged into {target_id}", body.source_id)),
            );
            Ok(())
        })?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitCluster {
    pub face_ids: Vec<i64>,
    pub label: Option<String>,
}

pub async fn split_cluster(
    State(state): Ctx,
    Path(cluster_id): Path<i64>,
    Json(body): Json<SplitCluster>,
) -> ApiResult<Json<i64>> {
    if body.face_ids.is_empty() {
        return Err(ApiError::bad_request("select the faces to split out first"));
    }
    let label = body.label.unwrap_or_else(|| "Unknown Person (split)".to_string());
    let face_ids = body.face_ids;

    let core = Arc::clone(&state.core);
    let new_id = blocking(move || {
        let new_id = core.db.transaction(|conn| {
            let new_id = clusters::split(conn, cluster_id, &face_ids, &label)?;
            logs::record_quiet(
                conn,
                logs::EVENT_CLUSTER_SPLIT,
                None,
                None,
                None,
                Some(&format!("{} faces split from cluster {cluster_id}", face_ids.len())),
            );
            Ok(new_id)
        })?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(new_id)
    })
    .await?;
    Ok(Json(new_id))
}

pub async fn ignore_cluster(
    State(state): Ctx,
    Path(cluster_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        clusters::set_status(&conn, cluster_id, ClusterStatus::Ignored)?;
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Albums
// ---------------------------------------------------------------------------

pub async fn list_albums(State(state): Ctx, Query(scope): Query<ShootRef>) -> ApiResult<Json<Vec<Album>>> {
    let core = Arc::clone(&state.core);
    let found = blocking(move || {
        let conn = core.db.conn()?;
        Ok(albums::list(&conn, scope.shoot_id)?)
    })
    .await?;
    Ok(Json(found))
}

pub async fn regenerate_albums(State(state): Ctx, Json(body): Json<ShootRef>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let created = blocking(move || {
        let created = core.db.transaction(|conn| albums::regenerate(conn, body.shoot_id))?;
        events::shoot_changed(core.sink(), body.shoot_id, "albums");
        Ok(created)
    })
    .await?;
    Ok(Json(created))
}

// ---------------------------------------------------------------------------
// Groups — the editor's own sorting (§34)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupStats {
    pub media_total: i64,
    pub grouped: i64,
    pub ungrouped: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedResult {
    pub groups: usize,
    pub files: usize,
}

pub async fn list_groups(State(state): Ctx, Query(scope): Query<ShootRef>) -> ApiResult<Json<Vec<Group>>> {
    let core = Arc::clone(&state.core);
    let found = blocking(move || {
        let conn = core.db.conn()?;
        Ok(groups::list(&conn, scope.shoot_id)?)
    })
    .await?;
    Ok(Json(found))
}

pub async fn group_stats(State(state): Ctx, Query(scope): Query<ShootRef>) -> ApiResult<Json<GroupStats>> {
    let core = Arc::clone(&state.core);
    let stats = blocking(move || {
        let conn = core.db.conn()?;
        let media_total = media_repo::count_for_shoot(&conn, scope.shoot_id)?;
        let ungrouped = groups::ungrouped_count(&conn, scope.shoot_id)?;
        Ok(GroupStats { media_total, grouped: media_total - ungrouped, ungrouped })
    })
    .await?;
    Ok(Json(stats))
}

pub async fn group_links(
    State(state): Ctx,
    Query(scope): Query<ShootRef>,
) -> ApiResult<Json<Vec<MediaGroupLink>>> {
    let core = Arc::clone(&state.core);
    let links = blocking(move || {
        let conn = core.db.conn()?;
        Ok(groups::links(&conn, scope.shoot_id)?)
    })
    .await?;
    Ok(Json(links))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGroup {
    pub shoot_id: i64,
    pub name: String,
}

pub async fn create_group(State(state): Ctx, Json(body): Json<CreateGroup>) -> ApiResult<Json<Group>> {
    let core = Arc::clone(&state.core);
    let group = blocking(move || {
        let conn = core.db.conn()?;
        let group = groups::get_or_create(&conn, body.shoot_id, &body.name, None)?;
        logs::record_quiet(
            &conn,
            logs::EVENT_GROUP_CREATED,
            Some(body.shoot_id),
            None,
            None,
            Some(&group.name),
        );
        events::shoot_changed(core.sink(), body.shoot_id, "groups");
        Ok(group)
    })
    .await?;
    Ok(Json(group))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGroup {
    pub name: Option<String>,
    pub folder_name: Option<String>,
    pub notes: Option<String>,
}

pub async fn update_group(
    State(state): Ctx,
    Path(group_id): Path<i64>,
    Json(body): Json<UpdateGroup>,
) -> ApiResult<Json<Group>> {
    let core = Arc::clone(&state.core);
    let group = blocking(move || {
        let conn = core.db.conn()?;
        if let Some(name) = body.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            groups::rename(&conn, group_id, name)?;
            logs::record_quiet(&conn, logs::EVENT_GROUP_RENAMED, None, None, None, Some(name));
        }
        groups::update(&conn, group_id, body.folder_name.as_deref(), body.notes.as_deref())?;
        let group = groups::get_by_id(&conn, group_id)?
            .ok_or_else(|| ApiError::not_found("that group no longer exists"))?;
        events::shoot_changed(core.sink(), group.shoot_id, "groups");
        Ok(group)
    })
    .await?;
    Ok(Json(group))
}

pub async fn delete_group(
    State(state): Ctx,
    Path(group_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        let Some(group) = groups::get_by_id(&conn, group_id)? else { return Ok(()) };
        groups::delete(&conn, group_id)?;
        logs::record_quiet(
            &conn,
            logs::EVENT_GROUP_DELETED,
            Some(group.shoot_id),
            None,
            None,
            Some(&group.name),
        );
        events::shoot_changed(core.sink(), group.shoot_id, "groups");
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMediaBody {
    pub shoot_id: i64,
    /// An existing group…
    pub group_id: Option<i64>,
    /// …or a name to file under, created if it is new. The desktop command
    /// accepts either, so the route does too.
    pub group_name: Option<String>,
    pub media_ids: Vec<i64>,
    #[serde(default)]
    pub move_files: bool,
}

pub async fn add_media_to_group(
    State(state): Ctx,
    Json(body): Json<GroupMediaBody>,
) -> ApiResult<Json<usize>> {
    if body.media_ids.is_empty() {
        return Err(ApiError::bad_request("select some files first"));
    }
    let core = Arc::clone(&state.core);
    let added = blocking(move || {
        let (group, added) = core.db.transaction(|conn| {
            let group = match (body.group_id, body.group_name.as_deref()) {
                (Some(id), _) => groups::get_by_id(conn, id)?
                    .ok_or_else(|| teo_database::DbError::other("that group no longer exists"))?,
                (None, Some(name)) => groups::get_or_create(conn, body.shoot_id, name, None)?,
                (None, None) => {
                    return Err(teo_database::DbError::other("choose a group or type a new name"))
                }
            };
            let added = if body.move_files {
                groups::move_media(conn, group.id, &body.media_ids)?
            } else {
                groups::add_media(conn, group.id, &body.media_ids)?
            };
            Ok((group, added))
        })?;

        let conn = core.db.conn()?;
        logs::record_quiet(
            &conn,
            logs::EVENT_GROUP_ASSIGNMENT,
            Some(group.shoot_id),
            None,
            None,
            Some(&format!("{} file(s) sorted into {}", body.media_ids.len(), group.name)),
        );
        events::shoot_changed(core.sink(), group.shoot_id, "groups");
        Ok(added)
    })
    .await?;
    Ok(Json(added))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaIds {
    pub media_ids: Vec<i64>,
}

pub async fn remove_media_from_group(
    State(state): Ctx,
    Path(group_id): Path<i64>,
    Json(body): Json<MediaIds>,
) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let removed = blocking(move || {
        let conn = core.db.conn()?;
        let removed = groups::remove_media(&conn, group_id, &body.media_ids)?;
        if let Some(group) = groups::get_by_id(&conn, group_id)? {
            events::shoot_changed(core.sink(), group.shoot_id, "groups");
        }
        Ok(removed)
    })
    .await?;
    Ok(Json(removed))
}

pub async fn clear_group(State(state): Ctx, Path(group_id): Path<i64>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let removed = blocking(move || {
        let conn = core.db.conn()?;
        let removed = groups::clear(&conn, group_id)?;
        if let Some(group) = groups::get_by_id(&conn, group_id)? {
            events::shoot_changed(core.sink(), group.shoot_id, "groups");
        }
        Ok(removed)
    })
    .await?;
    Ok(Json(removed))
}

pub async fn groups_from_ai_albums(
    State(state): Ctx,
    Json(body): Json<ShootRef>,
) -> ApiResult<Json<SeedResult>> {
    let core = Arc::clone(&state.core);
    let result = blocking(move || {
        let (groups_touched, files) =
            core.db.transaction(|conn| groups::seed_from_player_albums(conn, body.shoot_id))?;

        let conn = core.db.conn()?;
        logs::record_quiet(
            &conn,
            logs::EVENT_GROUP_ASSIGNMENT,
            Some(body.shoot_id),
            None,
            None,
            Some(&format!("{groups_touched} group(s) seeded from AI albums with {files} file(s)")),
        );
        events::shoot_changed(core.sink(), body.shoot_id, "groups");
        Ok(SeedResult { groups: groups_touched, files })
    })
    .await?;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupFromAlbum {
    pub album_id: i64,
    pub name: Option<String>,
}

pub async fn group_from_album(
    State(state): Ctx,
    Json(body): Json<GroupFromAlbum>,
) -> ApiResult<Json<Group>> {
    let core = Arc::clone(&state.core);
    let group = blocking(move || {
        let group = core.db.transaction(|conn| {
            let album = albums::get_by_id(conn, body.album_id)?
                .ok_or_else(|| teo_database::DbError::other("that album no longer exists"))?;
            let label = body
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&album.name);
            let group =
                groups::get_or_create(conn, album.shoot_id, label, album.person_ids.first().copied())?;
            groups::add_media(conn, group.id, &albums::media_ids(conn, body.album_id, None)?)?;
            groups::get_by_id(conn, group.id)?
                .ok_or_else(|| teo_database::DbError::other("that group no longer exists"))
        })?;
        events::shoot_changed(core.sink(), group.shoot_id, "groups");
        Ok(group)
    })
    .await?;
    Ok(Json(group))
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

pub async fn list_faces(
    State(state): Ctx,
    Query(query): Query<FaceQuery>,
) -> ApiResult<Json<Vec<FaceWithContext>>> {
    let core = Arc::clone(&state.core);
    let found = blocking(move || {
        let conn = core.db.conn()?;
        Ok(faces::query(&conn, &query)?)
    })
    .await?;
    Ok(Json(found))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceIds {
    pub face_ids: Vec<i64>,
}

pub async fn confirm_faces(State(state): Ctx, Json(body): Json<FaceIds>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let updated = blocking(move || {
        let n = core.db.transaction(|conn| {
            let n = faces::confirm_many(conn, &body.face_ids)?;
            logs::record_quiet(
                conn,
                logs::EVENT_PLAYER_ASSIGNMENT,
                None,
                None,
                None,
                Some(&format!("{n} face(s) confirmed")),
            );
            Ok(n)
        })?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(n)
    })
    .await?;
    Ok(Json(updated))
}

/// "Wrong person" — sends the faces back to the unknown pool.
pub async fn reject_faces(State(state): Ctx, Json(body): Json<FaceIds>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let updated = blocking(move || {
        let n = core.db.transaction(|conn| {
            let n = faces::reject_many(conn, &body.face_ids)?;
            logs::record_quiet(
                conn,
                logs::EVENT_MANUAL_CORRECTION,
                None,
                None,
                None,
                Some(&format!("{n} suggestion(s) rejected")),
            );
            Ok(n)
        })?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(n)
    })
    .await?;
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignFaces {
    pub face_ids: Vec<i64>,
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
}

/// Bulk-assigns selected faces to a player (§10). A correction made here
/// becomes a library sample, which is how the user's fix improves future
/// recognition (§6).
pub async fn assign_faces(State(state): Ctx, Json(body): Json<AssignFaces>) -> ApiResult<Json<usize>> {
    if body.face_ids.is_empty() {
        return Err(ApiError::bad_request("select at least one face"));
    }
    let core = Arc::clone(&state.core);
    let updated = blocking(move || {
        let n = core.db.transaction(|conn| {
            let person_id = match (body.person_id, body.person_name.as_deref()) {
                (Some(id), _) => id,
                (None, Some(name)) => people::get_or_create(conn, name, None)?.id,
                (None, None) => return Err(teo_database::DbError::other("choose or name a player")),
            };
            let n = faces::assign_many(conn, &body.face_ids, person_id)?;
            logs::record_quiet(
                conn,
                logs::EVENT_MANUAL_CORRECTION,
                None,
                None,
                Some(person_id),
                Some(&format!("{n} face(s) assigned manually")),
            );
            Ok(n)
        })?;
        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        Ok(n)
    })
    .await?;
    Ok(Json(updated))
}

/// "Remove false face detection" — keeps the row but takes it out of every
/// count and album.
pub async fn ignore_faces(State(state): Ctx, Json(body): Json<FaceIds>) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let updated = blocking(move || {
        let updated = core.db.transaction(|conn| faces::ignore_many(conn, &body.face_ids))?;

        // Face counts on the affected images are now stale.
        let conn = core.db.conn()?;
        for face_id in &body.face_ids {
            if let Some(face) = faces::get_by_id(&conn, *face_id)? {
                media_repo::refresh_face_count(&conn, face.media_id)?;
            }
        }
        Ok(updated)
    })
    .await?;
    Ok(Json(updated))
}

/// What naming one face turned into.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameFaceResult {
    pub person: Person,
    /// Faces now assigned to them — more than one when the face belonged to a
    /// cluster, because the cluster is the same person everywhere it appears.
    pub faces_named: usize,
    /// Their group, created if this is the first time they have been named.
    pub group: Group,
    /// Files gathered into that group by this naming.
    pub files_added: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameFace {
    pub face_id: i64,
    pub name: String,
    pub team: Option<String>,
}

/// Names the person in one face, and gathers their footage.
///
/// This is the whole sorting loop in a single call, because doing it in four
/// round trips leaves the library half-updated when one of them fails:
///
/// 1. the name becomes a player, reused if that name is already known;
/// 2. every face in the same unknown cluster is assigned to them — a cluster is
///    one person by construction, so naming one face names them everywhere the
///    clusterer found them;
/// 3. albums are regenerated, which is what knows every file a player appears
///    in;
/// 4. a group named after them is created or topped up from that album.
///
/// Step 4 is the point: the answer to "who is this?" is only useful once their
/// footage is sitting in a folder of its own. Naming a second face in the same
/// photo does the same for that person, which is how a group photo becomes
/// several people's groups.
pub async fn name_face(State(state): Ctx, Json(body): Json<NameFace>) -> ApiResult<Json<NameFaceResult>> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("give the person a name"));
    }

    let core = Arc::clone(&state.core);
    let result = blocking(move || {
        let (person, faces_named, shoot_id) = core.db.transaction(|conn| {
            let face = faces::get_by_id(conn, body.face_id)?
                .ok_or_else(|| teo_database::DbError::other("that face is no longer in the library"))?;

            let person = people::get_or_create(conn, &name, body.team.as_deref())?;

            // A cluster is one person wherever it appears, so naming a face in
            // one names all of them. A face with no cluster — already reviewed,
            // or too far from anything else — is assigned on its own.
            let faces_named = match face.cluster_id {
                Some(cluster_id) => clusters::name_cluster(conn, cluster_id, person.id)?,
                None => faces::assign_many(conn, &[face.id], person.id)?,
            };

            logs::record_quiet(
                conn,
                logs::EVENT_PLAYER_ASSIGNMENT,
                Some(face.shoot_id),
                Some(face.media_id),
                Some(person.id),
                Some(&format!("named from a face; {faces_named} face(s) assigned")),
            );

            Ok((person, faces_named, face.shoot_id))
        })?;

        // Albums are derived from assignments, so they have to catch up before
        // they can say which files this player appears in.
        let (group, files_added) = core.db.transaction(|conn| {
            albums::regenerate(conn, shoot_id)?;

            let album = albums::list(conn, shoot_id)?.into_iter().find(|album| {
                album.album_type == AlbumType::Player.as_str() && album.person_ids.contains(&person.id)
            });

            let group = groups::get_or_create(conn, shoot_id, &person.name, Some(person.id))?;
            let files_added = match album {
                Some(album) => {
                    let media = albums::media_ids(conn, album.id, None)?;
                    groups::add_media(conn, group.id, &media)?
                }
                // No album means the assignment produced no visible media yet,
                // which is possible if every face on it was ignored.
                None => 0,
            };

            let group = groups::get_by_id(conn, group.id)?
                .ok_or_else(|| teo_database::DbError::other("that group no longer exists"))?;
            Ok((group, files_added))
        })?;

        events::emit(core.sink(), events::LIBRARY_CHANGED, ());
        events::shoot_changed(core.sink(), shoot_id, "groups");

        Ok(NameFaceResult { person, faces_named, group, files_added })
    })
    .await?;

    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreview {
    pub file_count: usize,
    pub total_bytes: u64,
    pub folders: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub shoot_id: i64,
    pub destination: String,
    pub options: ExportOptions,
}

/// Resolves a destination against the writable roots.
///
/// With no roots configured — a loopback server behind the desktop app, where a
/// native picker chose the folder — any path is allowed and the engine's own
/// "not inside the source" guard is the only check, exactly as on the desktop.
fn destination_within_roots(state: &ServerState, destination: &str) -> ApiResult<PathBuf> {
    let requested = PathBuf::from(destination);
    let roots = state.config.writable_roots();
    if roots.is_empty() {
        return Ok(requested);
    }
    Ok(resolve_new_within_roots(&requested, roots)?)
}

pub async fn preview_export(
    State(state): Ctx,
    Json(body): Json<ExportRequest>,
) -> ApiResult<Json<ExportPreview>> {
    let destination = destination_within_roots(&state, &body.destination)?;
    let core = Arc::clone(&state.core);
    let preview = blocking(move || {
        let plan = export::preview(&core.db, body.shoot_id, &destination, &body.options)?;
        Ok(ExportPreview {
            file_count: plan.len(),
            total_bytes: plan.total_bytes(),
            folders: plan.folders.clone(),
        })
    })
    .await?;
    Ok(Json(preview))
}

pub async fn start_export(State(state): Ctx, Json(body): Json<ExportRequest>) -> ApiResult<Json<i64>> {
    let destination = destination_within_roots(&state, &body.destination)?;
    let core = Arc::clone(&state.core);
    let export_id =
        blocking(move || Ok(export::start(core, body.shoot_id, destination, body.options)?)).await?;
    Ok(Json(export_id))
}

pub async fn cancel_export(State(state): Ctx, Json(body): Json<ShootRef>) -> Json<serde_json::Value> {
    state.core.cancel_shoot(body.shoot_id);
    Json(serde_json::json!({ "ok": true }))
}

pub async fn list_exports(
    State(state): Ctx,
    Query(scope): Query<ShootRef>,
) -> ApiResult<Json<Vec<ExportRecord>>> {
    let core = Arc::clone(&state.core);
    let history = blocking(move || {
        let conn = core.db.conn()?;
        Ok(exports::list(&conn, scope.shoot_id, 20)?)
    })
    .await?;
    Ok(Json(history))
}

// ---------------------------------------------------------------------------
// Logs and privacy (§24, §25)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogQuery {
    pub shoot_id: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn recent_logs(
    State(state): Ctx,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<Vec<LogEntry>>> {
    let core = Arc::clone(&state.core);
    let entries = blocking(move || {
        let conn = core.db.conn()?;
        Ok(logs::recent(&conn, query.shoot_id, query.limit.unwrap_or(200))?)
    })
    .await?;
    Ok(Json(entries))
}

/// Deletes every embedding while leaving detections and albums intact.
pub async fn clear_all_embeddings(State(state): Ctx) -> ApiResult<Json<usize>> {
    let core = Arc::clone(&state.core);
    let cleared = blocking(move || {
        let cleared = core.db.transaction(faces::clear_all_embeddings)?;
        let conn = core.db.conn()?;
        logs::record_quiet(
            &conn,
            logs::EVENT_RECOGNITION_DATA_CLEARED,
            None,
            None,
            None,
            Some(&format!("{cleared} embedding(s) deleted")),
        );
        events::notice(core.sink(), "success", format!("Deleted {cleared} face embeddings."));
        Ok(cleared)
    })
    .await?;
    Ok(Json(cleared))
}

/// The full reset: every face, cluster and player profile in the database.
pub async fn clear_all_recognition_data(State(state): Ctx) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        core.db.transaction(|conn| {
            conn.execute("DELETE FROM video_detections", [])?;
            conn.execute("DELETE FROM faces", [])?;
            conn.execute("DELETE FROM clusters", [])?;
            conn.execute("DELETE FROM albums", [])?;
            conn.execute("DELETE FROM people", [])?;
            conn.execute(
                "UPDATE media SET face_count = 0, person_count = 0, processing_status = 'indexed'",
                [],
            )?;
            Ok(())
        })?;

        let conn = core.db.conn()?;
        logs::record_quiet(&conn, logs::EVENT_RECOGNITION_DATA_CLEARED, None, None, None, Some("all"));
        events::notice(core.sink(), "success", "All recognition data has been deleted.");
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn clear_thumbnail_cache(State(state): Ctx) -> ApiResult<Json<u64>> {
    let core = Arc::clone(&state.core);
    let removed = blocking(move || {
        let removed = core.thumbnails.clear()?;
        let conn = core.db.conn()?;
        conn.execute("UPDATE media SET thumbnail_path = NULL", [])
            .map_err(teo_database::DbError::from)?;
        Ok(removed)
    })
    .await?;
    Ok(Json(removed))
}

pub async fn clear_log(State(state): Ctx) -> ApiResult<Json<serde_json::Value>> {
    let core = Arc::clone(&state.core);
    blocking(move || {
        let conn = core.db.conn()?;
        logs::clear(&conn)?;
        Ok(())
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
