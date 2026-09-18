//! Everything the React frontend can ask for, minus the front door.
//!
//! Commands stay thin: validate, call into a crate, return data. Anything
//! long-running is queued as a job; a front door that must not block runs a
//! command on a worker thread, which is why nothing here is async.

use skwad_database::Db;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use skwad_clustering::FaceMatcher;
use skwad_database::models::*;
use skwad_database::repo::{
    albums, clusters, exports, faces, groups, jobs, logs, media as media_repo, people, projects, shoots, telemetry,
    video,
};
use skwad_export_engine::ExportOptions;

use crate::events;
use crate::models::{ModelRegistry, ModelStatus};
use crate::settings::AppSettings;
use crate::stages;
use crate::state::AppState;

use crate::api::{ApiError, ApiError as CommandError, Ctx, Result};

fn err(message: impl Into<String>) -> CommandError {
    ApiError::bad_request(message)
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub paths: crate::paths::AppPaths,
    pub media_url_base: String,
    pub ffmpeg_available: bool,
    pub ffmpeg_version: Option<String>,
    pub gstreamer_available: bool,
    pub gstreamer_version: Option<String>,
    pub video_tracking_backend: String,
    pub models: ModelStatus,
    pub accelerators: Vec<skwad_face_detection::Accelerator>,
    pub cpu_cores: usize,
    pub supported_extensions: Vec<String>,
    pub cache_bytes: u64,
}

pub fn app_info(ctx: &Ctx) -> Result<AppInfo> {
    let settings = ctx.state.settings();
    let ffmpeg = crate::pipeline::discover_ffmpeg(&settings);
    let gstreamer = skwad_media_core::Gstreamer::discover();
    let registry = ModelRegistry::new(&ctx.state.paths.models);

    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        paths: ctx.state.paths.clone(),
        media_url_base: ctx.state.media_url_base.clone(),
        ffmpeg_available: ffmpeg.is_some(),
        ffmpeg_version: ffmpeg.as_ref().and_then(|f| f.version()),
        gstreamer_available: gstreamer.is_some(),
        gstreamer_version: gstreamer.as_ref().and_then(|runtime| runtime.version()),
        video_tracking_backend: match skwad_video_analysis::tracking::backend() {
            skwad_video_analysis::tracking::TrackingBackend::OpenCv => "OpenCV tracking",
            skwad_video_analysis::tracking::TrackingBackend::Disabled => "Detector only",
        }
        .to_string(),
        models: registry.status(settings.detector_model.as_deref(), settings.embedder_model.as_deref()),
        accelerators: skwad_face_detection::available_accelerators(),
        cpu_cores: num_cpus::get(),
        supported_extensions: skwad_media_core::formats::supported_extensions()
            .into_iter()
            .map(String::from)
            .collect(),
        cache_bytes: ctx.state.paths.cache_size(),
    })
}

pub fn get_settings(ctx: &Ctx) -> Result<AppSettings> {
    Ok(ctx.state.settings())
}

pub fn update_settings(ctx: &Ctx, settings: AppSettings) -> Result<AppSettings> {
    Ok(ctx.state.update_settings(settings)?)
}

pub fn model_status(ctx: &Ctx) -> Result<ModelStatus> {
    let settings = ctx.state.settings();
    Ok(ModelRegistry::new(&ctx.state.paths.models)
        .status(settings.detector_model.as_deref(), settings.embedder_model.as_deref()))
}

/// Which embedder the library is currently using, and how much of the
/// library was embedded by something else.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingCohorts {
    /// Content hash of the embedder in use; `None` when no embedder resolves.
    pub current_key: Option<String>,
    /// Embeddings produced by a different (or unknown) embedder. They cannot
    /// be compared with current ones, so the people in them are invisible to
    /// recognition until re-embedded.
    pub stale_faces: i64,
    pub stale_media: i64,
}

/// Reports embeddings that predate the current embedder — the "N faces were
/// embedded with an older model" line in Settings.
pub fn embedding_cohorts(ctx: &Ctx, shoot_id: Option<i64>) -> Result<EmbeddingCohorts> {
    let settings = ctx.state.settings();
    let current = ModelRegistry::new(&ctx.state.paths.models)
        .resolve_info(crate::models::ModelRole::Embedder, settings.embedder_model.as_deref())
        .map(|m| m.hash);
    let Some(current_key) = current else {
        return Ok(EmbeddingCohorts {
            current_key: None,
            stale_faces: 0,
            stale_media: 0,
        });
    };
    let mut conn = ctx.state.db.conn()?;
    let stale = faces::stale_embeddings(&mut conn, &current_key, shoot_id)?;
    Ok(EmbeddingCohorts {
        current_key: Some(current_key),
        stale_faces: stale.faces,
        stale_media: stale.media,
    })
}

/// Requeues analysis for every file whose embeddings came from an older
/// embedder, so the whole library ends up in one cohort again. Returns how
/// many files were queued.
pub fn reembed_stale_faces(
    ctx: &Ctx, shoot_id: Option<i64>) -> Result<usize> {
    let settings = ctx.state.settings();
    let current_key = ModelRegistry::new(&ctx.state.paths.models)
        .resolve_info(crate::models::ModelRole::Embedder, settings.embedder_model.as_deref())
        .map(|m| m.hash)
        .ok_or_else(|| err("no face embedder is installed, so nothing can be re-embedded"))?;
    let queued = stages::queue_reembed(&ctx.state.db, &current_key, shoot_id)?;
    for shoot in &queued.shoots {
        ctx.state.resume_shoot(*shoot);
        events::shoot_changed(ctx.sink.as_ref(), *shoot, "reembedding");
    }
    ctx.state.set_paused(false);
    Ok(queued.media)
}

// ---------------------------------------------------------------------------
// Shoots
// ---------------------------------------------------------------------------

pub fn list_shoots(ctx: &Ctx) -> Result<Vec<ShootSummary>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(shoots::list_summaries(&mut conn)?)
}

pub fn get_shoot(ctx: &Ctx, shoot_id: i64) -> Result<Option<ShootSummary>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(shoots::summary(&mut conn, shoot_id)?)
}

/// Creates a shoot and immediately queues the scan.
pub fn create_shoot(
    ctx: &Ctx,
    name: String,
    source_path: String,
) -> Result<Shoot> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(err("give the shoot a name"));
    }
    let source = PathBuf::from(&source_path);
    if !source.is_dir() {
        return Err(err(format!("{source_path} is not a folder")));
    }

    let shoot = {
        let mut conn = ctx.state.db.conn()?;
        let shoot = shoots::create(&mut conn, &name, &source_path)?;
        jobs::enqueue(&mut conn, shoot.id, JobKind::Scan, None, stages::priority::SCAN, None)?;
        shoot
    };

    ctx.state.resume_shoot(shoot.id);
    events::shoot_changed(ctx.sink.as_ref(), shoot.id, "created");
    Ok(shoot)
}

pub fn rename_shoot(ctx: &Ctx, shoot_id: i64, name: String) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err("give the shoot a name"));
    }
    let mut conn = ctx.state.db.conn()?;
    Ok(shoots::rename(&mut conn, shoot_id, name)?)
}

/// Removes the shoot's index. The user's media is not touched (§21).
pub fn delete_shoot_index(
    ctx: &Ctx, shoot_id: i64) -> Result<()> {
    ctx.state.cancel_shoot(shoot_id);
    let mut conn = ctx.state.db.conn()?;
    jobs::cancel_for_shoot(&mut conn, shoot_id)?;
    shoots::delete_index(&mut conn, shoot_id)?;
    logs::record_quiet(&mut conn, logs::EVENT_SHOOT_DELETED, Some(shoot_id), None, None, None);
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "deleted");
    Ok(())
}

/// Removes the indexes and unshared cached thumbnails/proxies for an explicit set of
/// shoots. The original source folders are read-only and are never traversed
/// or modified by this operation.
pub fn clear_selected_scanned_data(
    ctx: &Ctx,
    shoot_ids: Vec<i64>,
) -> Result<usize> {
    let mut shoot_ids: Vec<i64> = shoot_ids.into_iter().filter(|id| *id > 0).collect();
    shoot_ids.sort_unstable();
    shoot_ids.dedup();
    if shoot_ids.is_empty() {
        return Err(err("select at least one shoot to clear"));
    }

    for shoot_id in &shoot_ids {
        ctx.state.cancel_shoot(*shoot_id);
    }

    let (removed, mut thumbnail_paths, mut content_keys) = ctx.state.db.transaction(|conn| {
        let mut thumbnail_paths = Vec::<PathBuf>::new();
        let mut content_keys = Vec::<String>::new();
        // One statement for the whole selection rather than one per shoot:
        // `= ANY($1)` takes the id list directly, where the prepared-statement
        // loop cost a round trip per shoot.
        for row in conn.rows(
            "SELECT thumbnail_path, content_key FROM media WHERE shoot_id = ANY($1)",
            skwad_database::params![shoot_ids],
        )? {
            thumbnail_paths.extend(row.get::<_, Option<String>>(0).map(PathBuf::from));
            content_keys.push(row.get::<_, String>(1));
        }

        for shoot_id in &shoot_ids {
            jobs::cancel_for_shoot(conn, *shoot_id)?;
        }
        let removed = shoots::delete_indexes(conn, &shoot_ids)?;
        Ok((removed, thumbnail_paths, content_keys))
    })?;

    thumbnail_paths.sort_unstable();
    thumbnail_paths.dedup();
    let cache_root = ctx.state.thumbnails.root();
    let mut conn = ctx.state.db.conn()?;
    let mut thumbnails_removed = 0usize;
    for path in thumbnail_paths {
        let still_referenced: bool = conn
            .row_one(
                "SELECT EXISTS(SELECT 1 FROM media WHERE thumbnail_path = $1)",
                skwad_database::params![path.to_string_lossy().as_ref()],
            )?
            .get(0);
        if still_referenced || !path.starts_with(cache_root) || !path.is_file() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => thumbnails_removed += 1,
            Err(error) => tracing::warn!(file = %path.display(), %error, "could not remove an unused thumbnail"),
        }
    }
    content_keys.sort_unstable();
    content_keys.dedup();
    let mut proxies_removed = 0usize;
    let mut review_frames_removed = 0u64;
    for content_key in content_keys {
        let still_referenced: bool = conn
            .row_one(
                "SELECT EXISTS(SELECT 1 FROM media WHERE content_key = $1)",
                skwad_database::params![content_key],
            )?
            .get(0);
        if !still_referenced {
            if ctx.state.proxies.remove(&content_key)? {
                proxies_removed += 1;
            }
            review_frames_removed += ctx.state.video_frames.remove(&content_key)?;
        }
    }
    drop(conn);

    for shoot_id in &shoot_ids {
        events::shoot_changed(ctx.sink.as_ref(), *shoot_id, "deleted");
    }
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    tracing::info!(
        shoots = removed,
        thumbnails = thumbnails_removed,
        proxies = proxies_removed,
        review_frames = review_frames_removed,
        "cleared selected scanned data"
    );
    Ok(removed)
}

/// Removes all scanned shoot indexes and generated thumbnails/proxies while keeping
/// settings, player profiles, logs and installed models. Original media
/// folders are never modified.
pub fn clear_scanned_data(ctx: &Ctx) -> Result<usize> {
    ctx.state.set_paused(true);

    let cleared = (|| {
        let shoot_ids = {
            let mut conn = ctx.state.db.conn()?;
            shoots::list(&mut conn)?
                .into_iter()
                .map(|shoot| shoot.id)
                .collect::<Vec<_>>()
        };
        for shoot_id in shoot_ids {
            ctx.state.cancel_shoot(shoot_id);
        }
        ctx.state.db.transaction(shoots::clear_all_indexes)
    })();

    // Never leave processing paused if the database operation fails.
    ctx.state.set_paused(false);
    let removed = cleared?;

    let proxy_result = ctx.state.proxies.clear();
    match (ctx.state.thumbnails.clear(), proxy_result) {
        (Ok(thumbnails), Ok(proxies)) => {
            tracing::info!(shoots = removed, thumbnails, proxies, "cleared scanned data")
        }
        (Err(error), _) | (_, Err(error)) => {
            tracing::warn!(%error, "scan indexes were cleared but some cached previews could not be removed")
        }
    }

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(removed)
}

/// Re-scans the folder and queues anything unfinished — the "Resume
/// Processing" action.
pub fn resume_processing(
    ctx: &Ctx, shoot_id: i64) -> Result<usize> {
    ctx.state.resume_shoot(shoot_id);
    ctx.state.set_paused(false);

    let mut conn = ctx.state.db.conn()?;
    jobs::retry_failed(&mut conn, shoot_id)?;
    jobs::enqueue_unique(&mut conn, shoot_id, JobKind::Scan, None, stages::priority::SCAN)?;
    drop(conn);

    let queued = stages::queue_pending_work(&ctx.state.db, shoot_id)?;
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "resumed");
    Ok(queued)
}

pub fn pause_processing(ctx: &Ctx, shoot_id: i64, paused: bool) -> Result<bool> {
    ctx.state.set_shoot_paused(shoot_id, paused);
    Ok(paused)
}

pub fn cancel_processing(
    ctx: &Ctx, shoot_id: i64) -> Result<usize> {
    ctx.state.cancel_shoot(shoot_id);
    let mut conn = ctx.state.db.conn()?;
    let cancelled = jobs::cancel_for_shoot(&mut conn, shoot_id)?;
    shoots::set_status(&mut conn, shoot_id, ShootStatus::Paused)?;
    telemetry::cancel_active(&mut conn, shoot_id)?;
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "cancelled");
    Ok(cancelled)
}

/// Throws away all AI results for the shoot and starts the analysis again.
/// Used after changing a model or a threshold.
pub fn reanalyse_shoot(
    ctx: &Ctx, shoot_id: i64) -> Result<usize> {
    {
        let mut conn = ctx.state.db.conn()?;
        telemetry::cancel_active(&mut conn, shoot_id)?;
    }
    stages::reset_analysis(&ctx.state.db, shoot_id)?;
    ctx.state.resume_shoot(shoot_id);
    let queued = stages::queue_pending_work(&ctx.state.db, shoot_id)?;
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "reanalysing");
    Ok(queued)
}

pub fn get_progress(ctx: &Ctx, shoot_id: i64) -> Result<ProcessingProgress> {
    let mut conn = ctx.state.db.conn()?;
    let mut progress = jobs::progress(&mut conn, shoot_id)?;
    if let Some(blockage) = ctx.state.blockage(shoot_id) {
        progress.blocked_kind = Some(blockage.kind);
        progress.blocked_reason = Some(blockage.reason);
    }
    Ok(progress)
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

pub fn list_projects(ctx: &Ctx) -> Result<Vec<Project>> {
    let (account_id, email, organisation) = crate::api::catalogue::current_project_identity(ctx)?;
    let mut conn = ctx.state.db.conn()?;
    Ok(projects::list_accessible(
        &mut conn,
        &account_id,
        &email,
        organisation.as_deref(),
    )?)
}

pub fn save_project(ctx: &Ctx, project: Project) -> Result<Project> {
    let (account_id, email, organisation) = crate::api::catalogue::current_project_identity(ctx)?;
    ctx.state
        .db
        .transaction(|conn| projects::save(conn, &project, &account_id, &email, organisation.as_deref()))?;
    let mut conn = ctx.state.db.conn()?;
    projects::get(&mut conn, &project.id, &account_id, &email, organisation.as_deref())?
        .ok_or_else(|| err("the project could not be loaded after saving"))
}

pub fn delete_project(ctx: &Ctx, project_id: String) -> Result<()> {
    let (account_id, _, _) = crate::api::catalogue::current_project_identity(ctx)?;
    let mut conn = ctx.state.db.conn()?;
    Ok(projects::delete(&mut conn, &project_id, &account_id)?)
}

pub fn replace_project_members(
    ctx: &Ctx,
    project_id: String,
    members: Vec<ProjectMember>,
) -> Result<Project> {
    let (account_id, email, organisation) = crate::api::catalogue::current_project_identity(ctx)?;
    // `replace_members` runs its own transaction, so it takes the `Database`
    // rather than a connection. Under SQLite it took a plain connection and
    // opened a transaction on it with `unchecked_transaction`, which meant
    // wrapping this call in `db.transaction` was rejected as a nested BEGIN.
    projects::replace_members(&ctx.state.db, &project_id, &members, &account_id, &email)?;
    let mut conn = ctx.state.db.conn()?;
    projects::get(&mut conn, &project_id, &account_id, &email, organisation.as_deref())?
        .ok_or_else(|| err("the project could not be loaded after sharing"))
}

pub fn get_shoot_telemetry(ctx: &Ctx, shoot_id: i64) -> Result<ShootTelemetry> {
    let mut conn = ctx.state.db.conn()?;
    Ok(telemetry::latest(&mut conn, shoot_id)?)
}

pub fn list_failed_jobs(ctx: &Ctx, shoot_id: i64) -> Result<Vec<Job>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(jobs::list_failed(&mut conn, shoot_id, 200)?)
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

pub fn list_media(ctx: &Ctx, query: MediaQuery) -> Result<Vec<Media>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(media_repo::query(&mut conn, &query)?)
}

pub fn get_media(ctx: &Ctx, media_id: i64) -> Result<Option<Media>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(media_repo::get_by_id(&mut conn, media_id)?)
}

pub fn media_faces(ctx: &Ctx, media_id: i64) -> Result<Vec<Face>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(faces::for_media(&mut conn, media_id)?)
}

/// Stores editor-owned stars and pick/reject flags. This accepts multiple ids
/// so the Sort screen can rate a selection with one keystroke.
pub fn set_media_editorial(
    ctx: &Ctx,
    media_ids: Vec<i64>,
    rating: Option<i64>,
    pick_state: Option<String>,
) -> Result<usize> {
    if media_ids.is_empty() {
        return Err(err("select at least one file to rate"));
    }

    let mut conn = ctx.state.db.conn()?;
    let mut shoot_ids = Vec::new();
    for media_id in &media_ids {
        let media = media_repo::get_by_id(&mut conn, *media_id)?
            .ok_or_else(|| err(format!("media {media_id} no longer exists")))?;
        shoot_ids.push(media.shoot_id);
    }
    shoot_ids.sort_unstable();
    shoot_ids.dedup();

    let changed = media_repo::set_editorial_state(&mut conn, &media_ids, rating, pick_state.as_deref())?;
    for shoot_id in shoot_ids {
        events::shoot_changed(ctx.sink.as_ref(), shoot_id, "editorial");
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Players
// ---------------------------------------------------------------------------

pub fn list_people(ctx: &Ctx, shoot_id: Option<i64>) -> Result<Vec<PersonSummary>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(people::list_summaries(&mut conn, shoot_id)?)
}

/// The hidden shoot enrollment reference photos/video are parked in, or
/// `None` if nobody has enrolled yet. The Pre-Process tab uses this to keep
/// reference samples visually separate from media actually found by search
/// (`exclude_shoot_id` in `MediaQuery`) — never creates the shoot as a side
/// effect of merely checking.
pub fn reference_library_shoot_id(ctx: &Ctx) -> Result<Option<i64>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(shoots::reference_library_id(&mut conn)?)
}

/// The Pre-Process tab's list: only people enrolled by name + reference
/// photo/video, not everyone in the library (see `people::list_enrolled_summaries`).
/// Returns an empty list rather than creating the reference shoot when nobody
/// has enrolled yet.
pub fn list_enrolled_people(ctx: &Ctx) -> Result<Vec<PersonSummary>> {
    let mut conn = ctx.state.db.conn()?;
    match shoots::reference_library_id(&mut conn)? {
        Some(reference_shoot_id) => Ok(people::list_enrolled_summaries(&mut conn, reference_shoot_id)?),
        None => Ok(Vec::new()),
    }
}

pub fn create_person(
    ctx: &Ctx,
    name: String,
    team: Option<String>,
) -> Result<Person> {
    let mut conn = ctx.state.db.conn()?;
    let person = people::get_or_create(&mut conn, &name, team.as_deref())?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_PLAYER_CREATED,
        None,
        None,
        Some(person.id),
        Some(&person.name),
    );
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(person)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollPersonResult {
    pub person: Person,
    pub samples_added: usize,
    pub rejected_count: usize,
}

/// One reference face ready to be written: either a still photo with exactly
/// one detected face, or one sampled frame from a reference video.
struct ReferenceSample {
    media_id: i64,
    bbox: BoundingBox,
    embedding: Vec<f32>,
    quality: f64,
    frame_time: Option<f64>,
    /// The embedder that produced `embedding`; see `faces.model_key`.
    model_key: String,
}

/// Pre-registers a person from reference photos or a reference video, taken
/// outside of any shoot. Reference material is parked in the hidden
/// "Reference Library" shoot (`shoots::get_or_create_reference_library`)
/// because every `faces`/`media` row requires a real `shoot_id`.
///
/// Every accepted sample is written straight to `assignment = 'confirmed'`:
/// unlike a detected face, a reference photo the user deliberately chose for
/// this person *is* the ground truth, not a hypothesis to review. This alone
/// does not touch any other shoot — matching already-processed media is a
/// separate, explicit step (`find_person_media`), and future shoots pick this
/// person up automatically the next time `recognise_shoot` runs, because it
/// reads the same confirmed-faces library this writes into.
pub fn enroll_person(
    ctx: &Ctx,
    name: String,
    team: Option<String>,
    photo_paths: Vec<String>,
    video_path: Option<String>,
) -> Result<EnrollPersonResult> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(err("give this person a name"));
    }
    if photo_paths.is_empty() && video_path.is_none() {
        return Err(err("add at least 3 reference photos or one reference video"));
    }
    if !photo_paths.is_empty() && photo_paths.len() < 3 {
        return Err(err("add at least 3 reference photos"));
    }
    if !photo_paths.is_empty() && video_path.is_some() {
        return Err(err("use either reference photos or a reference video, not both"));
    }

    #[allow(clippy::redundant_closure_call)]
    let result = (move || -> Result<EnrollPersonResult> {
        let settings = ctx.state.settings();
        let mut engine = crate::pipeline::Engine::new(&ctx.state.paths, &settings)?;

        let person = {
            let mut conn = ctx.state.db.conn()?;
            people::get_or_create(&mut conn, &name, team.as_deref())?
        };
        let reference_shoot = {
            let mut conn = ctx.state.db.conn()?;
            shoots::get_or_create_reference_library(&mut conn)?
        };

        let mut samples: Vec<ReferenceSample> = Vec::new();
        let mut rejected = 0usize;

        for photo_path in &photo_paths {
            match reference_sample_from_photo(&ctx.state, &mut engine, &settings, reference_shoot.id, photo_path)? {
                Some(sample) => samples.push(sample),
                None => rejected += 1,
            }
        }

        if let Some(video_path) = &video_path {
            let ffmpeg = engine
                .ffmpeg()
                .cloned()
                .ok_or_else(|| err("FFmpeg is required to enroll a person from a video"))?;
            let path = PathBuf::from(video_path);

            let media_id = {
                let mut conn = ctx.state.db.conn()?;
                media_repo::upsert(
                    &mut conn,
                    &NewMedia {
                        shoot_id: reference_shoot.id,
                        path: video_path.clone(),
                        filename: path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| video_path.clone()),
                        media_type: MediaType::Video,
                        extension: path
                            .extension()
                            .map(|e| e.to_string_lossy().to_lowercase())
                            .unwrap_or_default(),
                        file_size: std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0),
                        content_key: video_path.clone(),
                        captured_at: None,
                        normalized_relative_path: None,
                    },
                )?
            };
            let mut item = {
                let mut conn = ctx.state.db.conn()?;
                media_repo::get_by_id(&mut conn, media_id)?
                    .ok_or_else(|| err("the reference video could not be indexed"))?
            };
            // Populates width/height/duration/orientation so the sampler below
            // can plan against real dimensions, exactly like a scanned video.
            crate::pipeline::index_media(&ctx.state.db, &ctx.state.thumbnails, engine.ffmpeg(), &item)?;
            item = {
                let mut conn = ctx.state.db.conn()?;
                media_repo::get_by_id(&mut conn, media_id)?
                    .ok_or_else(|| err("the reference video could not be indexed"))?
            };

            let orientation = item.orientation.clamp(1, 8) as u16;
            let dimensions = item
                .width
                .zip(item.height)
                .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)));
            let video_config = settings.video_config();
            let plan = skwad_video_analysis::plan_video(&ffmpeg, &path, item.duration, dimensions, &video_config);
            let sampled = skwad_video_analysis::sample_frames(&ffmpeg, &path, &plan, orientation, &video_config);

            let mut frame_samples: Vec<ReferenceSample> = Vec::new();
            for frame in sampled {
                let mut detected = engine.detect_and_embed(&frame.image)?;
                detected.retain(|face| face.embedding.is_some());
                if detected.len() != 1 {
                    continue;
                }
                let face = detected.pop().expect("checked length above");
                let (width, height) = frame.image.dimensions();
                let (x, y, w, h) = face.detection.bbox.normalised(width, height);
                frame_samples.push(ReferenceSample {
                    media_id,
                    bbox: BoundingBox { x, y, w, h },
                    embedding: face.embedding.expect("filtered above").into_vec(),
                    quality: face.detection.quality(width, height),
                    frame_time: Some(frame.timestamp),
                    model_key: engine.embedder_key().to_string(),
                });
            }

            // Keep only the best few frames — the same cap normal recognition
            // applies per person (`stages::MAX_ENROLLMENT_VIDEO_SAMPLES`).
            frame_samples.sort_by(|a, b| b.quality.total_cmp(&a.quality));
            frame_samples.truncate(crate::stages::MAX_ENROLLMENT_VIDEO_SAMPLES);
            if frame_samples.is_empty() {
                rejected += 1;
            }
            samples.extend(frame_samples);
        }

        if samples.is_empty() {
            return Err(err(
                "no usable reference face was found — try clearer, front-facing photos or a video",
            ));
        }

        let samples_added = samples.len();
        write_reference_samples(&ctx.state, person.id, reference_shoot.id, &samples)?;

        Ok(EnrollPersonResult {
            person,
            samples_added,
            rejected_count: rejected,
        })
    })()?;

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(result)
}

/// Turns one reference photo into a sample, indexing it into the Reference
/// Library on the way. `None` means the photo was unusable — unreadable, no
/// face, or more than one face — and belongs in the rejected count rather
/// than being guessed at.
fn reference_sample_from_photo(
    state: &AppState,
    engine: &mut crate::pipeline::Engine,
    settings: &AppSettings,
    reference_shoot_id: i64,
    photo_path: &str,
) -> Result<Option<ReferenceSample>> {
    let path = PathBuf::from(photo_path);
    let orientation = skwad_media_core::metadata::read_orientation(
        &path,
        skwad_media_core::formats::MediaKind::Photo,
        engine.ffmpeg(),
    )
    .unwrap_or(1);
    let decoded = match skwad_media_core::decode::decode_image(
        &path,
        orientation,
        Some(settings.analysis_max_dim),
        engine.ffmpeg(),
    ) {
        Ok(decoded) => decoded,
        Err(error) => {
            tracing::warn!(file = %photo_path, %error, "could not read a reference photo");
            return Ok(None);
        }
    };

    let mut detected = engine.detect_and_embed(&decoded.image)?;
    detected.retain(|face| face.embedding.is_some());
    if detected.len() != 1 {
        return Ok(None);
    }
    let face = detected.pop().expect("checked length above");
    let (width, height) = decoded.image.dimensions();
    let (x, y, w, h) = face.detection.bbox.normalised(width, height);
    let quality = face.detection.quality(width, height);

    let media_id = {
        let mut conn = state.db.conn()?;
        media_repo::upsert(
            &mut conn,
            &NewMedia {
                shoot_id: reference_shoot_id,
                path: photo_path.to_string(),
                filename: path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| photo_path.to_string()),
                media_type: MediaType::Photo,
                extension: path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default(),
                file_size: std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0),
                content_key: photo_path.to_string(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )?
    };
    let item = {
        let mut conn = state.db.conn()?;
        media_repo::get_by_id(&mut conn, media_id)?.ok_or_else(|| err("the reference photo could not be indexed"))?
    };
    crate::pipeline::index_media(&state.db, &state.thumbnails, engine.ffmpeg(), &item)?;

    Ok(Some(ReferenceSample {
        media_id,
        bbox: BoundingBox { x, y, w, h },
        embedding: face.embedding.expect("filtered above").into_vec(),
        quality,
        frame_time: None,
        model_key: engine.embedder_key().to_string(),
    }))
}

/// Writes accepted reference faces as this person's confirmed ground truth.
fn write_reference_samples(
    state: &AppState,
    person_id: i64,
    reference_shoot_id: i64,
    samples: &[ReferenceSample],
) -> Result<()> {
    let samples_added = samples.len();
    let mut media_ids: Vec<i64> = samples.iter().map(|s| s.media_id).collect();
    media_ids.sort_unstable();
    media_ids.dedup();

    Ok(state.db.transaction(|conn| {
        for sample in samples {
            let face_id = faces::insert_manual(
                conn,
                &NewFace {
                    media_id: sample.media_id,
                    shoot_id: reference_shoot_id,
                    bbox: sample.bbox,
                    landmarks: None,
                    detection_confidence: 1.0,
                    embedding: Some(sample.embedding.clone()),
                    quality: Some(sample.quality),
                    frame_time: sample.frame_time,
                    crop_path: None,
                    model_key: Some(sample.model_key.clone()),
                },
            )?;
            faces::assign(conn, face_id, person_id, Some(1.0))?;
        }
        for media_id in &media_ids {
            media_repo::refresh_face_count(conn, *media_id)?;
        }
        logs::record_quiet(
            conn,
            logs::EVENT_PLAYER_CREATED,
            None,
            None,
            Some(person_id),
            Some(&format!("enrolled with {samples_added} reference sample(s)")),
        );
        Ok(())
    })?)
}

/// The angle subfolders a reference directory is expected to hold. One person
/// per filename, the same filename in each folder.
const REFERENCE_ANGLE_FOLDERS: [&str; 3] = ["front", "left", "right"];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolledFromDirectory {
    pub name: String,
    /// Which angle folders this person was found in, e.g. `["front", "left"]`.
    pub angles: Vec<String>,
    pub samples_added: usize,
    /// Photos with zero or more than one detected face, skipped rather than guessed.
    pub rejected_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollDirectoryResult {
    pub enrolled: Vec<EnrolledFromDirectory>,
    /// People whose photos yielded no usable face at all, so nothing was written.
    pub skipped: Vec<String>,
}

/// One person's reference photos, gathered from the angle folders they appear in.
#[derive(Debug)]
struct RosterEntry {
    name: String,
    /// `(angle folder, photo path)`, in front/left/right order.
    photos: Vec<(&'static str, String)>,
}

/// Reads a `front`/`left`/`right` reference folder into one entry per person,
/// keyed on the filename stem so the same name across angle folders is one
/// person. Angle folders are matched case-insensitively, non-photos are
/// ignored, and a person present in only some of the folders is still
/// returned with whatever angles exist — a missing profile shot is worth
/// reporting, not worth failing the whole roster over.
fn reference_roster(root: &std::path::Path) -> Result<Vec<RosterEntry>> {
    if !root.is_dir() {
        return Err(err("choose a folder that exists"));
    }

    let mut angle_dirs: Vec<(&'static str, PathBuf)> = Vec::new();
    for angle in REFERENCE_ANGLE_FOLDERS {
        let entries = std::fs::read_dir(root).map_err(|e| err(format!("could not read that folder: {e}")))?;
        let found = entries
            .flatten()
            .find(|entry| entry.path().is_dir() && entry.file_name().to_string_lossy().eq_ignore_ascii_case(angle));
        if let Some(entry) = found {
            angle_dirs.push((angle, entry.path()));
        }
    }
    if angle_dirs.is_empty() {
        return Err(err(
            "that folder has no front, left or right subfolder — expected one folder per angle, with the same filename per person in each",
        ));
    }

    // BTreeMap keeps the roster alphabetical; the lowercased stem is the key
    // so "Naresh.png" and "naresh.jpg" across angles land on one person.
    let mut roster: std::collections::BTreeMap<String, RosterEntry> = std::collections::BTreeMap::new();
    for (angle, dir) in &angle_dirs {
        let entries = std::fs::read_dir(dir).map_err(|e| err(format!("could not read the {angle} folder: {e}")))?;
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        // read_dir order is filesystem-defined; sort so a roster import is
        // reproducible run to run.
        paths.sort();
        for path in paths {
            if !path.is_file() {
                continue;
            }
            let is_photo = skwad_media_core::formats::classify(&path)
                .is_some_and(|(kind, _)| kind == skwad_media_core::formats::MediaKind::Photo);
            if !is_photo {
                continue;
            }
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().trim().to_string()) else {
                continue;
            };
            if stem.is_empty() {
                continue;
            }
            roster
                .entry(stem.to_lowercase())
                .or_insert_with(|| RosterEntry {
                    name: stem.clone(),
                    photos: Vec::new(),
                })
                .photos
                .push((angle, path.to_string_lossy().to_string()));
        }
    }
    if roster.is_empty() {
        return Err(err("no photos were found in those angle folders"));
    }
    Ok(roster.into_values().collect())
}

/// Bulk-enrolls a whole roster from one directory laid out as
/// `front/`, `left/` and `right/`, where the same filename in each folder is
/// the same person (`front/naresh.png`, `left/naresh.png`, …). The filename
/// stem becomes the person's name.
///
/// Like single-person enrollment this deliberately bypasses the scan/analyse
/// job pipeline: reference headshots are ground truth the user curated, so
/// faces are detected inline and written straight to `confirmed`. Nothing is
/// imported as a shoot and no processing jobs are queued.
pub fn enroll_people_from_directory(
    ctx: &Ctx,
    root: String,
    team: Option<String>,
) -> Result<EnrollDirectoryResult> {
    let roster = reference_roster(std::path::Path::new(&root))?;

    #[allow(clippy::redundant_closure_call)]
    let result = (move || -> Result<EnrollDirectoryResult> {
        let settings = ctx.state.settings();
        let mut engine = crate::pipeline::Engine::new(&ctx.state.paths, &settings)?;
        let reference_shoot = {
            let mut conn = ctx.state.db.conn()?;
            shoots::get_or_create_reference_library(&mut conn)?
        };

        let mut enrolled = Vec::new();
        let mut skipped = Vec::new();

        for entry in roster {
            let mut samples = Vec::new();
            let mut rejected = 0usize;
            for (_angle, photo_path) in &entry.photos {
                match reference_sample_from_photo(&ctx.state, &mut engine, &settings, reference_shoot.id, photo_path)? {
                    Some(sample) => samples.push(sample),
                    None => rejected += 1,
                }
            }
            if samples.is_empty() {
                skipped.push(entry.name);
                continue;
            }

            let person = {
                let mut conn = ctx.state.db.conn()?;
                people::get_or_create(&mut conn, &entry.name, team.as_deref())?
            };
            let samples_added = samples.len();
            write_reference_samples(&ctx.state, person.id, reference_shoot.id, &samples)?;
            enrolled.push(EnrolledFromDirectory {
                name: person.name,
                angles: entry.photos.iter().map(|(angle, _)| angle.to_string()).collect(),
                samples_added,
                rejected_count: rejected,
            });
        }

        Ok(EnrollDirectoryResult { enrolled, skipped })
    })()?;

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(result)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchPersonReport {
    pub shoots_scanned: usize,
    pub new_suggestions: usize,
}

/// On-demand retroactive matching: checks one pre-registered person's
/// reference samples against media that was already processed before they
/// were enrolled. Results land as suggestions for review, exactly like any
/// other recognition result — nothing is added to a collection or project
/// automatically (see `stages::match_person_in_shoot`).
pub fn find_person_media(
    ctx: &Ctx,
    person_id: i64,
    shoot_id: Option<i64>,
) -> Result<MatchPersonReport> {
    #[allow(clippy::redundant_closure_call)]
    let (result, changed_shoots) =
        (move || -> Result<(MatchPersonReport, Vec<i64>)> {
            let settings = ctx.state.settings();
            let shoot_ids: Vec<i64> = match shoot_id {
                Some(id) => vec![id],
                None => {
                    let mut conn = ctx.state.db.conn()?;
                    shoots::list(&mut conn)?.into_iter().map(|s| s.id).collect()
                }
            };

            let mut new_suggestions = 0usize;
            let mut changed_shoots = Vec::new();
            for id in &shoot_ids {
                let matched = stages::match_person_in_shoot(&ctx.state.db, *id, person_id, &settings)?;
                new_suggestions += matched;
                if matched > 0 {
                    changed_shoots.push(*id);
                }
            }

            Ok((
                MatchPersonReport {
                    shoots_scanned: shoot_ids.len(),
                    new_suggestions,
                },
                changed_shoots,
            ))
        })()?;

    if !changed_shoots.is_empty() {
        events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
        for shoot_id in changed_shoots {
            events::shoot_changed(ctx.sink.as_ref(), shoot_id, "personMatched");
        }
    }
    Ok(result)
}

pub fn rename_person(
    ctx: &Ctx, person_id: i64, name: String) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    people::rename(&mut conn, person_id, &name)?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_PLAYER_RENAMED,
        None,
        None,
        Some(person_id),
        Some(&name),
    );
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(())
}

pub fn update_person(
    ctx: &Ctx,
    person_id: i64,
    team: Option<String>,
    notes: Option<String>,
) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    Ok(people::update(&mut conn, person_id, team.as_deref(), notes.as_deref())?)
}

/// Folds one player into another (§10, "Merge two people").
pub fn merge_people(
    ctx: &Ctx, target_id: i64, source_id: i64) -> Result<i64> {
    let moved = ctx.state.db.transaction(|conn| people::merge(conn, target_id, source_id))?;
    let mut conn = ctx.state.db.conn()?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_PLAYER_MERGED,
        None,
        None,
        Some(target_id),
        Some(&format!("absorbed player {source_id}; {moved} faces now on the target")),
    );
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(moved)
}

pub fn delete_person(
    ctx: &Ctx, person_id: i64) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    people::delete(&mut conn, person_id)?;
    logs::record_quiet(&mut conn, logs::EVENT_PLAYER_DELETED, None, None, Some(person_id), None);
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(())
}

/// Drops a player's biometric data but keeps the profile (§22, §24).
pub fn clear_person_recognition(
    ctx: &Ctx, person_id: i64) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    people::clear_recognition_data(&mut conn, person_id)?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_RECOGNITION_DATA_CLEARED,
        None,
        None,
        Some(person_id),
        None,
    );
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(())
}

// ---------------------------------------------------------------------------
// Clusters
// ---------------------------------------------------------------------------

pub fn list_clusters(
    ctx: &Ctx,
    shoot_id: i64,
    include_named: bool,
) -> Result<Vec<ClusterSummary>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(clusters::list_summaries(&mut conn, shoot_id, include_named)?)
}

/// Names an unknown cluster, promoting it to a player and adding every face in
/// it to the library (§7).
pub fn name_cluster(
    ctx: &Ctx,
    cluster_id: i64,
    name: String,
    team: Option<String>,
) -> Result<Person> {
    let person = ctx.state.db.transaction(|conn| {
        let person = people::get_or_create(conn, &name, team.as_deref())?;
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

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(person)
}

pub fn merge_clusters(
    ctx: &Ctx, target_id: i64, source_id: i64) -> Result<()> {
    ctx.state.db.transaction(|conn| {
        clusters::merge(conn, target_id, source_id)?;
        logs::record_quiet(
            conn,
            logs::EVENT_CLUSTER_MERGED,
            None,
            None,
            None,
            Some(&format!("cluster {source_id} merged into {target_id}")),
        );
        Ok(())
    })?;
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(())
}

/// Pulls faces out of a cluster into a new one (§10, "Split incorrect cluster").
pub fn split_cluster(
    ctx: &Ctx,
    cluster_id: i64,
    face_ids: Vec<i64>,
    label: Option<String>,
) -> Result<i64> {
    if face_ids.is_empty() {
        return Err(err("select the faces to split out first"));
    }
    let label = label.unwrap_or_else(|| "Unknown Person (split)".to_string());

    let new_id = ctx.state.db.transaction(|conn| {
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

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(new_id)
}

pub fn ignore_cluster(ctx: &Ctx, cluster_id: i64) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    Ok(clusters::set_status(&mut conn, cluster_id, ClusterStatus::Ignored)?)
}

// ---------------------------------------------------------------------------
// Albums
// ---------------------------------------------------------------------------

pub fn list_albums(ctx: &Ctx, shoot_id: i64) -> Result<Vec<Album>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(albums::list(&mut conn, shoot_id)?)
}

pub fn regenerate_albums(
    ctx: &Ctx, shoot_id: i64) -> Result<usize> {
    let created = ctx.state.db.transaction(|conn| {
        media_repo::refresh_duplicate_groups(conn, shoot_id, 6)?;
        albums::regenerate(conn, shoot_id)
    })?;
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "albums");
    Ok(created)
}

// ---------------------------------------------------------------------------
// Groups — the editor's own sorting (§34)
// ---------------------------------------------------------------------------

/// The counters the sorting screen shows above the grid: how much of the shoot
/// has been filed, and how much is still waiting.
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

pub fn list_groups(ctx: &Ctx, shoot_id: i64) -> Result<Vec<Group>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(groups::list(&mut conn, shoot_id)?)
}

pub fn group_stats(ctx: &Ctx, shoot_id: i64) -> Result<GroupStats> {
    let mut conn = ctx.state.db.conn()?;
    let media_total = media_repo::count_for_shoot(&mut conn, shoot_id)?;
    let ungrouped = groups::ungrouped_count(&mut conn, shoot_id)?;
    Ok(GroupStats {
        media_total,
        grouped: media_total - ungrouped,
        ungrouped,
    })
}

/// Which groups hold which files, for the chips drawn on each thumbnail.
pub fn group_links(ctx: &Ctx, shoot_id: i64) -> Result<Vec<MediaGroupLink>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(groups::links(&mut conn, shoot_id)?)
}

/// Creates the group the editor just named. The name is what the export folder
/// will be called, which is why it is validated here rather than at export
/// time — a bad name should fail while the person who typed it is looking.
pub fn create_group(
    ctx: &Ctx, shoot_id: i64, name: String) -> Result<Group> {
    let mut conn = ctx.state.db.conn()?;
    let group = groups::get_or_create(&mut conn, shoot_id, &name, None)?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_GROUP_CREATED,
        Some(shoot_id),
        None,
        None,
        Some(&group.name),
    );
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "groups");
    Ok(group)
}

pub fn rename_group(
    ctx: &Ctx, group_id: i64, name: String) -> Result<Group> {
    let mut conn = ctx.state.db.conn()?;
    groups::rename(&mut conn, group_id, &name)?;
    let group = groups::get_by_id(&mut conn, group_id)?.ok_or_else(|| err("that group no longer exists"))?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_GROUP_RENAMED,
        Some(group.shoot_id),
        None,
        None,
        Some(&group.name),
    );
    events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    Ok(group)
}

/// Sets the on-disk folder name and the note. A blank folder name goes back to
/// using the group's own name.
pub fn update_group(
    ctx: &Ctx,
    group_id: i64,
    folder_name: Option<String>,
    notes: Option<String>,
) -> Result<Group> {
    let mut conn = ctx.state.db.conn()?;
    groups::update(&mut conn, group_id, folder_name.as_deref(), notes.as_deref())?;
    let group = groups::get_by_id(&mut conn, group_id)?.ok_or_else(|| err("that group no longer exists"))?;
    events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    Ok(group)
}

/// Deletes a group. Only the grouping is lost — no file is touched.
pub fn delete_group(
    ctx: &Ctx, group_id: i64) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    let Some(group) = groups::get_by_id(&mut conn, group_id)? else {
        return Ok(());
    };
    groups::delete(&mut conn, group_id)?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_GROUP_DELETED,
        Some(group.shoot_id),
        None,
        None,
        Some(&group.name),
    );
    events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    Ok(())
}

/// Files the selected media into a group, creating it from `group_name` if the
/// editor typed a new one.
///
/// `move_files` takes them out of every other group in the shoot first — the
/// fix for something filed under the wrong player. Without it a file can
/// legitimately belong to several groups, which is what a clip with two players
/// in it needs.
pub fn add_media_to_group(
    ctx: &Ctx,
    shoot_id: i64,
    group_id: Option<i64>,
    group_name: Option<String>,
    media_ids: Vec<i64>,
    move_files: bool,
) -> Result<usize> {
    if media_ids.is_empty() {
        return Err(err("select some files first"));
    }

    let (group, added) = ctx.state.db.transaction(|conn| {
        let group = match (group_id, group_name.as_deref()) {
            (Some(id), _) => groups::get_by_id(conn, id)?
                .ok_or_else(|| skwad_database::DbError::other("that group no longer exists"))?,
            (None, Some(name)) => groups::get_or_create(conn, shoot_id, name, None)?,
            (None, None) => return Err(skwad_database::DbError::other("choose a group or type a new name")),
        };
        let added = if move_files {
            groups::move_media(conn, group.id, &media_ids)?
        } else {
            groups::add_media(conn, group.id, &media_ids)?
        };
        Ok((group, added))
    })?;

    let mut conn = ctx.state.db.conn()?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_GROUP_ASSIGNMENT,
        Some(group.shoot_id),
        None,
        None,
        Some(&format!("{} file(s) sorted into {}", media_ids.len(), group.name)),
    );
    events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    Ok(added)
}

pub fn remove_media_from_group(
    ctx: &Ctx,
    group_id: i64,
    media_ids: Vec<i64>,
) -> Result<usize> {
    let mut conn = ctx.state.db.conn()?;
    let removed = groups::remove_media(&mut conn, group_id, &media_ids)?;
    if let Some(group) = groups::get_by_id(&mut conn, group_id)? {
        events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    }
    Ok(removed)
}

pub fn clear_group(
    ctx: &Ctx, group_id: i64) -> Result<usize> {
    let mut conn = ctx.state.db.conn()?;
    let removed = groups::clear(&mut conn, group_id)?;
    if let Some(group) = groups::get_by_id(&mut conn, group_id)? {
        events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    }
    Ok(removed)
}

/// The head start: one group per player the AI identified, pre-filled with that
/// player's album, so the editor corrects rather than sorts from scratch.
///
/// Running it again after naming more faces tops the groups up; it never undoes
/// a manual edit.
pub fn groups_from_ai_albums(
    ctx: &Ctx, shoot_id: i64) -> Result<SeedResult> {
    let (groups_touched, files) = ctx.state
        .db
        .transaction(|conn| groups::seed_from_player_albums(conn, shoot_id))?;

    let mut conn = ctx.state.db.conn()?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_GROUP_ASSIGNMENT,
        Some(shoot_id),
        None,
        None,
        Some(&format!(
            "{groups_touched} group(s) seeded from AI albums with {files} file(s)"
        )),
    );
    events::shoot_changed(ctx.sink.as_ref(), shoot_id, "groups");
    Ok(SeedResult {
        groups: groups_touched,
        files,
    })
}

/// Turns one AI album into an editable group — the "this one is right, I will
/// fix the rest by hand" path from the Albums screen.
pub fn group_from_album(
    ctx: &Ctx,
    album_id: i64,
    name: Option<String>,
) -> Result<Group> {
    let group = ctx.state.db.transaction(|conn| {
        let album = albums::get_by_id(conn, album_id)?
            .ok_or_else(|| skwad_database::DbError::other("that album no longer exists"))?;
        let label = name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&album.name);
        let group = groups::get_or_create(conn, album.shoot_id, label, album.person_ids.first().copied())?;
        {
            let ids = albums::media_ids(conn, album_id, None)?;
            groups::add_media(conn, group.id, &ids)?
        };
        groups::get_by_id(conn, group.id)?.ok_or_else(|| skwad_database::DbError::other("that group no longer exists"))
    })?;

    events::shoot_changed(ctx.sink.as_ref(), group.shoot_id, "groups");
    Ok(group)
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

pub fn list_faces(ctx: &Ctx, query: FaceQuery) -> Result<Vec<FaceWithContext>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(faces::query(&mut conn, &query)?)
}

/// Accepts the AI's suggestion for these faces.
pub fn confirm_faces(
    ctx: &Ctx, face_ids: Vec<i64>) -> Result<usize> {
    let updated = ctx.state.db.transaction(|conn| {
        let n = faces::confirm_many(conn, &face_ids)?;
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
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(updated)
}

/// "Wrong person" — sends the faces back to the unknown pool.
pub fn reject_faces(
    ctx: &Ctx, face_ids: Vec<i64>) -> Result<usize> {
    let updated = ctx.state.db.transaction(|conn| {
        let n = faces::reject_many(conn, &face_ids)?;
        video::sync_face_people(conn, &face_ids)?;
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
    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(updated)
}

/// Bulk-assigns selected faces to a player (§10). A correction made here
/// becomes a library sample, which is how the user's fix improves future
/// recognition (§6).
pub fn assign_faces(
    ctx: &Ctx,
    face_ids: Vec<i64>,
    person_id: Option<i64>,
    person_name: Option<String>,
) -> Result<usize> {
    if face_ids.is_empty() {
        return Err(err("select at least one face"));
    }

    let updated = ctx.state.db.transaction(|conn| {
        let person_id = match (person_id, person_name.as_deref()) {
            (Some(id), _) => id,
            // A name on an imported roster brings its team with it, so bulk
            // assignment files the player the same way naming one face does.
            (None, Some(name)) => {
                let team = skwad_database::repo::roster::resolve(conn, name)?.map(|entry| entry.team);
                people::get_or_create(conn, name, team.as_deref())?.id
            }
            (None, None) => return Err(skwad_database::DbError::other("choose or name a player")),
        };
        let n = faces::assign_many(conn, &face_ids, person_id)?;
        video::sync_face_people(conn, &face_ids)?;
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

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    Ok(updated)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualFaceResult {
    pub face: Face,
    pub suggested_person: Option<Person>,
}

fn validate_manual_bbox(bbox: BoundingBox) -> Result<BoundingBox> {
    if !bbox.x.is_finite() || !bbox.y.is_finite() || !bbox.w.is_finite() || !bbox.h.is_finite() {
        return Err(err("the face box contains invalid coordinates"));
    }

    let x1 = bbox.x.clamp(0.0, 1.0);
    let y1 = bbox.y.clamp(0.0, 1.0);
    let x2 = (bbox.x + bbox.w).clamp(0.0, 1.0);
    let y2 = (bbox.y + bbox.h).clamp(0.0, 1.0);
    let clean = BoundingBox {
        x: x1,
        y: y1,
        w: x2 - x1,
        h: y2 - y1,
    };
    if clean.w < 0.005 || clean.h < 0.005 {
        return Err(err("draw a larger box around the face"));
    }
    Ok(clean)
}

/// Turns a reviewer-drawn box into a real face record: decode the original,
/// extract an embedding from the crop, compare it with confirmed named faces,
/// then return the suggestion for human review. Model work runs off the UI
/// thread because loading and inference can take a moment.
pub fn add_manual_face(
    ctx: &Ctx,
    media_id: i64,
    bbox: BoundingBox,
    frame_time: Option<f64>,
) -> Result<ManualFaceResult> {
    let bbox = validate_manual_bbox(bbox)?;
    #[allow(clippy::redundant_closure_call)]
    let result = (move || -> Result<ManualFaceResult> {
        let media = {
            let mut conn = ctx.state.db.conn()?;
            media_repo::get_by_id(&mut conn, media_id)?.ok_or_else(|| err("that file is no longer in the library"))?
        };
        let frame_time = if media.media_type == MediaType::Video.as_str() {
            let timestamp = frame_time
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or_else(|| err("choose an analysed video sample before marking a face"))?;
            Some(
                media
                    .duration
                    .map_or(timestamp, |duration| timestamp.min(duration.max(0.0))),
            )
        } else {
            None
        };

        let settings = ctx.state.settings();
        let mut engine = crate::pipeline::Engine::new(&ctx.state.paths, &settings)?;
        let (embedding, quality) = engine.embed_manual_face(&media, bbox, frame_time)?;

        let (library, used_people) = {
            let mut conn = ctx.state.db.conn()?;
            let used_people: Vec<i64> = faces::for_media(&mut conn, media.id)?
                .into_iter()
                .filter(|face| face.assignment != FaceAssignment::Ignored.as_str())
                .filter(|face| match frame_time {
                    Some(timestamp) => face.frame_time.is_some_and(|at| (at - timestamp).abs() < 0.01),
                    None => true,
                })
                .filter_map(|face| face.person_id)
                .collect();
            (faces::library_vectors(&mut conn)?, used_people)
        };
        let matcher = FaceMatcher::build(library.into_iter().filter_map(|sample| {
            sample
                .person_id
                .filter(|person_id| !used_people.contains(person_id))
                .map(|person_id| (person_id, sample.embedding))
        }));
        let matched = matcher.match_one(&embedding, &settings.matcher_config());

        let result = ctx.state.db.transaction(|conn| {
            let face_id = faces::insert_manual(
                conn,
                &NewFace {
                    media_id: media.id,
                    shoot_id: media.shoot_id,
                    bbox,
                    landmarks: None,
                    detection_confidence: 1.0,
                    embedding: Some(embedding),
                    quality: Some(quality),
                    frame_time,
                    crop_path: None,
                    model_key: Some(engine.embedder_key().to_string()),
                },
            )?;

            if let Some(timestamp) = frame_time {
                video::insert(conn, media.id, None, Some(face_id), timestamp, 1.0)?;
            }

            let suggested_person = match matched {
                Some(matched) => {
                    faces::set_suggestion(conn, face_id, matched.person_id, matched.similarity as f64)?;
                    people::get_by_id(conn, matched.person_id)?
                }
                None => None,
            };
            media_repo::refresh_face_count(conn, media.id)?;
            logs::record_quiet(
                conn,
                logs::EVENT_MANUAL_CORRECTION,
                Some(media.shoot_id),
                Some(media.id),
                suggested_person.as_ref().map(|person| person.id),
                Some(if frame_time.is_some() {
                    "reviewer drew a missed face box on a video sample"
                } else {
                    "reviewer drew a missed face box"
                }),
            );
            let face = faces::get_by_id(conn, face_id)?
                .ok_or_else(|| skwad_database::DbError::other("the new face could not be read back"))?;
            Ok(ManualFaceResult { face, suggested_person })
        })?;
        Ok(result)
    })()?;

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    events::shoot_changed(ctx.sink.as_ref(), result.face.shoot_id, "manualFace");
    Ok(result)
}

/// Names the person in one face and gathers every currently known appearance
/// into an editor-owned group. Keeping the assignment, album refresh, and
/// grouping in one transaction prevents the UI from observing a half-named
/// person after a failure.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameFaceResult {
    pub person: Person,
    pub faces_named: usize,
    pub matches_found: usize,
    pub group: Group,
    pub files_added: usize,
    /// The team the roster matched this name to, when it matched one.
    pub team: Option<String>,
    /// The team's own group, which now holds this player's media alongside
    /// every other named player from the same team.
    pub team_group: Option<Group>,
}

pub fn name_face(
    ctx: &Ctx,
    face_id: i64,
    name: String,
    team: Option<String>,
) -> Result<NameFaceResult> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(err("give the person a name"));
    }

    #[allow(clippy::redundant_closure_call)]
    let result = (move || -> Result<NameFaceResult> {
        // An imported roster knows which team this name plays for. Whatever the
        // reviewer typed is looked up; an unambiguous hit supplies the team, and
        // anything else leaves the caller's own value alone.
        let roster_entry = {
            let mut conn = ctx.state.db.conn()?;
            skwad_database::repo::roster::resolve(&mut conn, &name)?
        };
        let team = team.or_else(|| roster_entry.as_ref().map(|entry| entry.team.clone()));

        let (person, faces_named, shoot_id, appearances_before_matching) = ctx.state.db.transaction(|conn| {
            let face = faces::get_by_id(conn, face_id)?
                .ok_or_else(|| skwad_database::DbError::other("that face is no longer in the library"))?;
            let person = people::get_or_create(conn, &name, team.as_deref())?;

            // Clicking one box confirms exactly that face. A machine-created
            // cluster is only a suggestion and may contain a lookalike; silently
            // confirming every member polluted both the group and the reusable
            // reference library. The remaining members will be re-evaluated by
            // the stricter matcher below and stay reviewable.
            if face.cluster_id.is_some() {
                faces::set_cluster(conn, face.id, None)?;
                clusters::refresh_counts(conn, face.shoot_id)?;
            }
            let faces_named = faces::assign_many(conn, &[face.id], person.id)?;

            logs::record_quiet(
                conn,
                logs::EVENT_PLAYER_ASSIGNMENT,
                Some(face.shoot_id),
                Some(face.media_id),
                Some(person.id),
                Some(&format!("named from photo; {faces_named} face(s) assigned")),
            );
            let appearances_before_matching: i64 = conn
                .row_one(
                    "SELECT COUNT(*) FROM faces
                      WHERE shoot_id = $1 AND person_id = $2
                        AND assignment IN ('suggested','confirmed')",
                    skwad_database::params![face.shoot_id, person.id],
                )?
                .get(0);
            Ok((person, faces_named, face.shoot_id, appearances_before_matching))
        })?;

        // The newly confirmed face is now a reference sample. Recognition may
        // have completed before the reviewer named it, so compare the remaining
        // unknown faces immediately. This is deliberately format-agnostic:
        // camera RAW, JPEG, PNG, HEIC and TIFF are all photo rows here.
        stages::recognise_shoot(&ctx.state.db, shoot_id, &ctx.state.settings())?;

        ctx.state
            .db
            .transaction(|conn| {
                let appearances_after_matching: i64 = conn
                    .row_one(
                        "SELECT COUNT(*) FROM faces
                          WHERE shoot_id = $1 AND person_id = $2
                            AND assignment IN ('suggested','confirmed')",
                        skwad_database::params![shoot_id, person.id],
                    )?
                    .get(0);
                let matches_found = appearances_after_matching
                    .saturating_sub(appearances_before_matching)
                    .max(0) as usize;
                albums::regenerate(conn, shoot_id)?;
                let player_album = albums::list(conn, shoot_id)?.into_iter().find(|album| {
                    album.album_type == AlbumType::Player.as_str() && album.person_ids.contains(&person.id)
                });

                let group = groups::get_or_create(conn, shoot_id, &person.name, Some(person.id))?;
                let files_added = match &player_album {
                    Some(album) => {
                        let ids = albums::media_ids(conn, album.id, None)?;
                        groups::add_media(conn, group.id, &ids)?
                    }
                    None => 0,
                };
                let group = groups::get_by_id(conn, group.id)?
                    .ok_or_else(|| skwad_database::DbError::other("that group no longer exists"))?;

                // Every player the roster places on a team shares one group, so
                // the team's media collects itself as faces are named. The group
                // carries no person id: it belongs to the team, not to anybody.
                let person_team = person.team.clone();
                let team_group = match person.team.as_deref().filter(|team| !team.trim().is_empty()) {
                    Some(team) => {
                        let team_group = groups::get_or_create(conn, shoot_id, team.trim(), None)?;
                        if let Some(album) = &player_album {
                            {
                                let ids = albums::media_ids(conn, album.id, None)?;
                                groups::add_media(conn, team_group.id, &ids)?
                            };
                        }
                        groups::get_by_id(conn, team_group.id)?
                    }
                    None => None,
                };

                Ok(NameFaceResult {
                    person,
                    faces_named,
                    matches_found,
                    group,
                    files_added,
                    team: person_team,
                    team_group,
                })
            })
            .map_err(CommandError::from)
    })()?;

    events::emit(ctx.sink.as_ref(), events::LIBRARY_CHANGED, ());
    events::shoot_changed(ctx.sink.as_ref(), result.group.shoot_id, "groups");
    Ok(result)
}

/// "Remove false face detection" — keeps the row but takes it out of every
/// count and album.
pub fn ignore_faces(ctx: &Ctx, face_ids: Vec<i64>) -> Result<usize> {
    let updated = ctx.state.db.transaction(|conn| {
        let updated = faces::ignore_many(conn, &face_ids)?;
        video::delete_for_faces(conn, &face_ids)?;
        Ok(updated)
    })?;

    // Face counts on the affected images are now stale.
    let mut conn = ctx.state.db.conn()?;
    for face_id in &face_ids {
        if let Some(face) = faces::get_by_id(&mut conn, *face_id)? {
            media_repo::refresh_face_count(&mut conn, face.media_id)?;
        }
    }
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

pub fn video_timelines(ctx: &Ctx, media_id: i64) -> Result<Vec<VideoTimeline>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(video::timelines(&mut conn, media_id)?)
}

pub fn video_sample_frames(ctx: &Ctx, media_id: i64) -> Result<Vec<f64>> {
    let mut conn = ctx.state.db.conn()?;
    let stored = video::sample_times(&mut conn, media_id)?;
    let duration = media_repo::get_by_id(&mut conn, media_id)?.and_then(|media| {
        (media.media_type == MediaType::Video.as_str())
            .then_some(media.duration)
            .flatten()
    });
    Ok(review_sample_times(stored, duration, &ctx.state.settings().video_config()))
}

fn review_sample_times(
    stored: Vec<f64>,
    duration: Option<f64>,
    config: &skwad_video_analysis::VideoAnalysisConfig,
) -> Vec<f64> {
    // The interval plan is inexpensive and deterministic, so existing videos
    // analysed before sample-frame indexing still expose every cadence frame.
    // Persisted scene-change samples are unioned in when they exist.
    let mut milliseconds = std::collections::BTreeSet::new();
    for timestamp in stored.into_iter().chain(
        skwad_video_analysis::plan_frames(duration, &[], config)
            .timestamps
            .into_iter()
            .map(|frame| frame.at),
    ) {
        if timestamp.is_finite() && timestamp >= 0.0 {
            milliseconds.insert((timestamp * 1_000.0).round() as i64);
        }
    }
    milliseconds.into_iter().map(|value| value as f64 / 1_000.0).collect()
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

pub fn preview_export(
    ctx: &Ctx,
    shoot_id: i64,
    destination: String,
    options: ExportOptions,
) -> Result<ExportPreview> {
    let plan = crate::export::preview(&ctx.state.db, shoot_id, std::path::Path::new(&destination), &options)?;
    Ok(ExportPreview {
        file_count: plan.len(),
        total_bytes: plan.total_bytes(),
        folders: plan.folders.clone(),
    })
}

pub fn start_export(
    ctx: &Ctx,
    shoot_id: i64,
    destination: String,
    options: ExportOptions,
) -> Result<i64> {
    Ok(crate::export::start(
        Arc::clone(&ctx.sink),
        Arc::clone(&ctx.state),
        shoot_id,
        PathBuf::from(destination),
        options,
    )?)
}

pub fn cancel_export(ctx: &Ctx, shoot_id: i64) -> Result<()> {
    ctx.state.cancel_shoot(shoot_id);
    Ok(())
}

pub fn list_exports(ctx: &Ctx, shoot_id: i64) -> Result<Vec<ExportRecord>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(exports::list(&mut conn, shoot_id, 20)?)
}

// ---------------------------------------------------------------------------
// Premiere (§ premiere_api) — queues a job for the UXP panel's next poll.
// Neither command reaches Premiere directly: only code running inside
// Premiere can call its scripting API.
// ---------------------------------------------------------------------------

pub fn send_media_to_premiere(
    ctx: &Ctx,
    media_ids: Vec<i64>,
    label: Option<String>,
) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    let mut files = Vec::new();
    for id in &media_ids {
        if let Some(item) = media_repo::get_by_id(&mut conn, *id)? {
            if std::path::Path::new(&item.path).is_file() {
                files.push(crate::state::PremiereJobFile {
                    is_video: item.media_type == "video",
                    path: item.path,
                    filename: item.filename,
                });
            }
        }
    }
    if files.is_empty() {
        return Err(err("none of the selected files could be found on disk"));
    }
    let label = label.unwrap_or_else(|| format!("{} file{}", files.len(), if files.len() == 1 { "" } else { "s" }));
    // No bin: a one-off file (or an ad-hoc multi-select) lands in the project
    // root. A Collection send (below) is the one case that gets its own bin.
    ctx.state.enqueue_premiere_job(label, None, files);
    Ok(())
}

pub fn send_collection_to_premiere(ctx: &Ctx, collection_id: String) -> Result<()> {
    let (account_id, email, organisation) = crate::api::catalogue::current_project_identity(ctx)?;
    let mut conn = ctx.state.db.conn()?;
    let accessible = projects::list_accessible(&mut conn, &account_id, &email, organisation.as_deref())?;
    drop(conn);

    let collection = accessible
        .into_iter()
        .flat_map(|project| project.collections)
        .find(|collection| collection.id == collection_id)
        .ok_or_else(|| err("collection not found"))?;

    let files: Vec<crate::state::PremiereJobFile> =
        crate::export::resolve_collection_files(&ctx.state.db, &collection.sources)?
            .into_iter()
            .map(|file| crate::state::PremiereJobFile {
                path: file.path.display().to_string(),
                filename: file.filename,
                is_video: file.is_video,
            })
            .collect();
    if files.is_empty() {
        return Err(err("this collection has no files to send"));
    }
    ctx.state.enqueue_premiere_job(collection.name.clone(), Some(collection.name), files);
    Ok(())
}

// ---------------------------------------------------------------------------
// Logs and privacy (§24, §25)
// ---------------------------------------------------------------------------

pub fn recent_logs(ctx: &Ctx, shoot_id: Option<i64>, limit: i64) -> Result<Vec<LogEntry>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(logs::recent(&mut conn, shoot_id, limit)?)
}

/// Deletes every embedding while leaving detections and albums intact.
pub fn clear_all_embeddings(ctx: &Ctx) -> Result<usize> {
    let cleared = ctx.state.db.transaction(faces::clear_all_embeddings)?;
    let mut conn = ctx.state.db.conn()?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_RECOGNITION_DATA_CLEARED,
        None,
        None,
        None,
        Some(&format!("{cleared} embedding(s) deleted")),
    );
    events::notice(ctx.sink.as_ref(), "success", format!("Deleted {cleared} face embeddings."));
    Ok(cleared)
}

/// The full reset: every face, cluster and player profile in the database.
pub fn clear_all_recognition_data(ctx: &Ctx) -> Result<()> {
    ctx.state.db.transaction(|conn| {
        conn.exec("DELETE FROM video_detections", skwad_database::params![])?;
        conn.exec("DELETE FROM faces", skwad_database::params![])?;
        conn.exec("DELETE FROM clusters", skwad_database::params![])?;
        conn.exec("DELETE FROM albums", skwad_database::params![])?;
        conn.exec("DELETE FROM people", skwad_database::params![])?;
        conn.exec(
            "UPDATE media SET face_count = 0, processing_status = 'indexed'",
            skwad_database::params![],
        )?;
        Ok(())
    })?;

    let mut conn = ctx.state.db.conn()?;
    logs::record_quiet(
        &mut conn,
        logs::EVENT_RECOGNITION_DATA_CLEARED,
        None,
        None,
        None,
        Some("all"),
    );
    events::notice(ctx.sink.as_ref(), "success", "All recognition data has been deleted.");
    Ok(())
}

pub fn clear_thumbnail_cache(ctx: &Ctx) -> Result<u64> {
    let removed = ctx.state.thumbnails.clear()? + ctx.state.proxies.clear()?;
    let mut conn = ctx.state.db.conn()?;
    conn.exec("UPDATE media SET thumbnail_path = NULL", skwad_database::params![])?;
    Ok(removed)
}

pub fn clear_log(ctx: &Ctx) -> Result<()> {
    let mut conn = ctx.state.db.conn()?;
    Ok(logs::clear(&mut conn)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_face_boxes_are_clamped_to_the_photo() {
        let bbox = validate_manual_bbox(BoundingBox {
            x: -0.1,
            y: 0.2,
            w: 0.4,
            h: 1.0,
        })
        .unwrap();
        assert_eq!(bbox.x, 0.0);
        assert_eq!(bbox.y, 0.2);
        assert!((bbox.w - 0.3).abs() < 1e-9);
        assert!((bbox.h - 0.8).abs() < 1e-9);
    }

    #[test]
    fn manual_face_boxes_reject_tiny_or_invalid_regions() {
        assert!(validate_manual_bbox(BoundingBox {
            x: 0.1,
            y: 0.1,
            w: 0.001,
            h: 0.2
        })
        .is_err());
        assert!(validate_manual_bbox(BoundingBox {
            x: f64::NAN,
            y: 0.1,
            w: 0.2,
            h: 0.2
        })
        .is_err());
    }

    #[test]
    fn video_review_keeps_scene_samples_and_fills_the_interval_cadence() {
        let config = skwad_video_analysis::VideoAnalysisConfig {
            sample_interval: 5.0,
            max_frames: 60,
            ..Default::default()
        };
        let samples = review_sample_times(vec![2.2, 10.0], Some(16.0), &config);

        assert_eq!(samples, vec![0.0, 2.2, 5.0, 10.0, 15.0]);
    }

    fn write_reference_photo(dir: &std::path::Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), b"not a real image").unwrap();
    }

    #[test]
    fn a_reference_folder_groups_each_name_across_its_angles() {
        let root = tempfile::tempdir().unwrap();
        for angle in ["front", "left", "right"] {
            write_reference_photo(&root.path().join(angle), "naresh.png");
            write_reference_photo(&root.path().join(angle), "mavi.jpg");
        }

        let roster = reference_roster(root.path()).unwrap();

        // Alphabetical, one entry per person, all three angles each.
        assert_eq!(
            roster.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["mavi", "naresh"]
        );
        for entry in &roster {
            assert_eq!(
                entry.photos.iter().map(|(angle, _)| *angle).collect::<Vec<_>>(),
                vec!["front", "left", "right"]
            );
        }
    }

    #[test]
    fn angle_folders_match_whatever_case_they_were_typed_in() {
        let root = tempfile::tempdir().unwrap();
        write_reference_photo(&root.path().join("Front"), "naresh.png");
        write_reference_photo(&root.path().join("LEFT"), "naresh.png");

        let roster = reference_roster(root.path()).unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].photos.len(), 2, "both angle folders should be read");
    }

    #[test]
    fn a_person_missing_an_angle_is_still_enrolled_with_the_rest() {
        let root = tempfile::tempdir().unwrap();
        write_reference_photo(&root.path().join("front"), "naresh.png");
        write_reference_photo(&root.path().join("left"), "naresh.png");
        write_reference_photo(&root.path().join("right"), "mavi.png");

        let roster = reference_roster(root.path()).unwrap();
        let naresh = roster.iter().find(|e| e.name == "naresh").unwrap();
        let mavi = roster.iter().find(|e| e.name == "mavi").unwrap();

        assert_eq!(
            naresh.photos.len(),
            2,
            "a missing profile shot must not drop the person"
        );
        assert_eq!(mavi.photos.len(), 1);
    }

    #[test]
    fn the_same_name_in_different_cases_or_formats_is_one_person() {
        let root = tempfile::tempdir().unwrap();
        write_reference_photo(&root.path().join("front"), "Naresh.png");
        write_reference_photo(&root.path().join("left"), "naresh.jpg");

        let roster = reference_roster(root.path()).unwrap();
        assert_eq!(roster.len(), 1, "case and extension must not split one person in two");
        assert_eq!(roster[0].photos.len(), 2);
    }

    #[test]
    fn non_photos_and_stray_folders_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        write_reference_photo(&root.path().join("front"), "naresh.png");
        write_reference_photo(&root.path().join("front"), "notes.txt");
        write_reference_photo(&root.path().join("front"), "clip.mp4");
        write_reference_photo(&root.path().join("blooper"), "someone.png");

        let roster = reference_roster(root.path()).unwrap();
        assert_eq!(
            roster.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["naresh"]
        );
    }

    #[test]
    fn a_folder_without_any_angle_subfolder_explains_the_expected_layout() {
        let root = tempfile::tempdir().unwrap();
        write_reference_photo(root.path(), "naresh.png");

        let error = reference_roster(root.path()).unwrap_err();
        assert!(error.message.contains("front"), "got: {}", error.message);
    }
}
