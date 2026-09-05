//! The Tauri IPC surface — everything the React frontend can ask for.
//!
//! Commands stay thin: validate, call into a crate, return data. Anything that
//! could take longer than a frame is queued as a job or spawned onto a thread
//! rather than run here, because a command blocks the caller's promise.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

use teo_clustering::FaceMatcher;
use teo_database::models::*;
use teo_database::repo::{
    albums, clusters, exports, faces, groups, jobs, logs, media as media_repo, people, shoots, video,
};
use teo_export_engine::ExportOptions;

use crate::events;
use crate::models::{ModelRegistry, ModelStatus};
use crate::settings::AppSettings;
use crate::stages;
use crate::state::AppState;

/// Errors cross the IPC boundary as a plain message; the UI shows it verbatim,
/// so the text has to be something a person can act on.
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
}

impl<E: std::fmt::Display> From<E> for CommandError {
    fn from(e: E) -> Self {
        CommandError { message: e.to_string() }
    }
}

pub type Result<T> = std::result::Result<T, CommandError>;

fn err(message: impl Into<String>) -> CommandError {
    CommandError {
        message: message.into(),
    }
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
    pub accelerators: Vec<teo_face_detection::Accelerator>,
    pub cpu_cores: usize,
    pub supported_extensions: Vec<String>,
    pub cache_bytes: u64,
}

#[tauri::command]
pub fn app_info(state: State<'_, Arc<AppState>>) -> Result<AppInfo> {
    let settings = state.settings();
    let ffmpeg = crate::pipeline::discover_ffmpeg(&settings);
    let gstreamer = teo_media_core::Gstreamer::discover();
    let registry = ModelRegistry::new(&state.paths.models);

    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        paths: state.paths.clone(),
        media_url_base: state.media_url_base.clone(),
        ffmpeg_available: ffmpeg.is_some(),
        ffmpeg_version: ffmpeg.as_ref().and_then(|f| f.version()),
        gstreamer_available: gstreamer.is_some(),
        gstreamer_version: gstreamer.as_ref().and_then(|runtime| runtime.version()),
        video_tracking_backend: match teo_video_analysis::tracking::backend() {
            teo_video_analysis::tracking::TrackingBackend::OpenCv => "OpenCV tracking",
            teo_video_analysis::tracking::TrackingBackend::Disabled => "Detector only",
        }
        .to_string(),
        models: registry.status(settings.detector_model.as_deref(), settings.embedder_model.as_deref()),
        accelerators: teo_face_detection::available_accelerators(),
        cpu_cores: num_cpus::get(),
        supported_extensions: teo_media_core::formats::supported_extensions()
            .into_iter()
            .map(String::from)
            .collect(),
        cache_bytes: state.paths.cache_size(),
    })
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Result<AppSettings> {
    Ok(state.settings())
}

#[tauri::command]
pub fn update_settings(state: State<'_, Arc<AppState>>, settings: AppSettings) -> Result<AppSettings> {
    Ok(state.update_settings(settings)?)
}

#[tauri::command]
pub fn model_status(state: State<'_, Arc<AppState>>) -> Result<ModelStatus> {
    let settings = state.settings();
    Ok(ModelRegistry::new(&state.paths.models)
        .status(settings.detector_model.as_deref(), settings.embedder_model.as_deref()))
}

// ---------------------------------------------------------------------------
// Shoots
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_shoots(state: State<'_, Arc<AppState>>) -> Result<Vec<ShootSummary>> {
    let conn = state.db.conn()?;
    Ok(shoots::list_summaries(&conn)?)
}

#[tauri::command]
pub fn get_shoot(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<Option<ShootSummary>> {
    let conn = state.db.conn()?;
    Ok(shoots::summary(&conn, shoot_id)?)
}

/// Creates a shoot and immediately queues the scan.
#[tauri::command]
pub fn create_shoot(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
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
        let conn = state.db.conn()?;
        let shoot = shoots::create(&conn, &name, &source_path)?;
        jobs::enqueue(&conn, shoot.id, JobKind::Scan, None, stages::priority::SCAN, None)?;
        shoot
    };

    state.resume_shoot(shoot.id);
    events::shoot_changed(&app, shoot.id, "created");
    Ok(shoot)
}

#[tauri::command]
pub fn rename_shoot(state: State<'_, Arc<AppState>>, shoot_id: i64, name: String) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err("give the shoot a name"));
    }
    let conn = state.db.conn()?;
    Ok(shoots::rename(&conn, shoot_id, name)?)
}

/// Removes the shoot's index. The user's media is not touched (§21).
#[tauri::command]
pub fn delete_shoot_index(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<()> {
    state.cancel_shoot(shoot_id);
    let conn = state.db.conn()?;
    jobs::cancel_for_shoot(&conn, shoot_id)?;
    shoots::delete_index(&conn, shoot_id)?;
    logs::record_quiet(&conn, logs::EVENT_SHOOT_DELETED, Some(shoot_id), None, None, None);
    events::shoot_changed(&app, shoot_id, "deleted");
    Ok(())
}

/// Removes the indexes and unshared cached thumbnails/proxies for an explicit set of
/// shoots. The original source folders are read-only and are never traversed
/// or modified by this operation.
#[tauri::command]
pub fn clear_selected_scanned_data(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    shoot_ids: Vec<i64>,
) -> Result<usize> {
    let mut shoot_ids: Vec<i64> = shoot_ids.into_iter().filter(|id| *id > 0).collect();
    shoot_ids.sort_unstable();
    shoot_ids.dedup();
    if shoot_ids.is_empty() {
        return Err(err("select at least one shoot to clear"));
    }

    for shoot_id in &shoot_ids {
        state.cancel_shoot(*shoot_id);
    }

    let (removed, mut thumbnail_paths, mut content_keys) = state.db.transaction(|conn| {
        let mut thumbnail_paths = Vec::<PathBuf>::new();
        let mut content_keys = Vec::<String>::new();
        {
            let mut statement = conn.prepare("SELECT thumbnail_path, content_key FROM media WHERE shoot_id = ?1")?;
            for shoot_id in &shoot_ids {
                let cached = statement
                    .query_map(teo_database::rusqlite::params![shoot_id], |row| {
                        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<teo_database::rusqlite::Result<Vec<_>>>()?;
                for (thumbnail_path, content_key) in cached {
                    thumbnail_paths.extend(thumbnail_path.map(PathBuf::from));
                    content_keys.push(content_key);
                }
            }
        }

        for shoot_id in &shoot_ids {
            jobs::cancel_for_shoot(conn, *shoot_id)?;
        }
        let removed = shoots::delete_indexes(conn, &shoot_ids)?;
        Ok((removed, thumbnail_paths, content_keys))
    })?;

    thumbnail_paths.sort_unstable();
    thumbnail_paths.dedup();
    let cache_root = state.thumbnails.root();
    let conn = state.db.conn()?;
    let mut thumbnails_removed = 0usize;
    for path in thumbnail_paths {
        let still_referenced: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM media WHERE thumbnail_path = ?1)",
            teo_database::rusqlite::params![path.to_string_lossy().as_ref()],
            |row| row.get(0),
        )?;
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
    for content_key in content_keys {
        let still_referenced: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM media WHERE content_key = ?1)",
            teo_database::rusqlite::params![content_key],
            |row| row.get(0),
        )?;
        if !still_referenced && state.proxies.remove(&content_key)? {
            proxies_removed += 1;
        }
    }
    drop(conn);

    for shoot_id in &shoot_ids {
        events::shoot_changed(&app, *shoot_id, "deleted");
    }
    events::emit(&app, events::LIBRARY_CHANGED, ());
    tracing::info!(
        shoots = removed,
        thumbnails = thumbnails_removed,
        proxies = proxies_removed,
        "cleared selected scanned data"
    );
    Ok(removed)
}

/// Removes all scanned shoot indexes and generated thumbnails/proxies while keeping
/// settings, player profiles, logs and installed models. Original media
/// folders are never modified.
#[tauri::command]
pub fn clear_scanned_data(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<usize> {
    state.set_paused(true);

    let cleared = (|| {
        let shoot_ids = {
            let conn = state.db.conn()?;
            shoots::list(&conn)?
                .into_iter()
                .map(|shoot| shoot.id)
                .collect::<Vec<_>>()
        };
        for shoot_id in shoot_ids {
            state.cancel_shoot(shoot_id);
        }
        state.db.transaction(shoots::clear_all_indexes)
    })();

    // Never leave processing paused if the database operation fails.
    state.set_paused(false);
    let removed = cleared?;

    let proxy_result = state.proxies.clear();
    match (state.thumbnails.clear(), proxy_result) {
        (Ok(thumbnails), Ok(proxies)) => {
            tracing::info!(shoots = removed, thumbnails, proxies, "cleared scanned data")
        }
        (Err(error), _) | (_, Err(error)) => {
            tracing::warn!(%error, "scan indexes were cleared but some cached previews could not be removed")
        }
    }

    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(removed)
}

/// Re-scans the folder and queues anything unfinished — the "Resume
/// Processing" action.
#[tauri::command]
pub fn resume_processing(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<usize> {
    state.resume_shoot(shoot_id);
    state.set_paused(false);

    let conn = state.db.conn()?;
    jobs::retry_failed(&conn, shoot_id)?;
    jobs::enqueue_unique(&conn, shoot_id, JobKind::Scan, None, stages::priority::SCAN)?;
    drop(conn);

    let queued = stages::queue_pending_work(&state.db, shoot_id)?;
    events::shoot_changed(&app, shoot_id, "resumed");
    Ok(queued)
}

#[tauri::command]
pub fn pause_processing(state: State<'_, Arc<AppState>>, paused: bool) -> Result<bool> {
    state.set_paused(paused);
    Ok(paused)
}

#[tauri::command]
pub fn cancel_processing(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<usize> {
    state.cancel_shoot(shoot_id);
    let conn = state.db.conn()?;
    let cancelled = jobs::cancel_for_shoot(&conn, shoot_id)?;
    shoots::set_status(&conn, shoot_id, ShootStatus::Paused)?;
    events::shoot_changed(&app, shoot_id, "cancelled");
    Ok(cancelled)
}

/// Throws away all AI results for the shoot and starts the analysis again.
/// Used after changing a model or a threshold.
#[tauri::command]
pub fn reanalyse_shoot(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<usize> {
    stages::reset_analysis(&state.db, shoot_id)?;
    state.resume_shoot(shoot_id);
    let queued = stages::queue_pending_work(&state.db, shoot_id)?;
    events::shoot_changed(&app, shoot_id, "reanalysing");
    Ok(queued)
}

#[tauri::command]
pub fn get_progress(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<ProcessingProgress> {
    let conn = state.db.conn()?;
    Ok(jobs::progress(&conn, shoot_id)?)
}

#[tauri::command]
pub fn list_failed_jobs(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<Vec<Job>> {
    let conn = state.db.conn()?;
    Ok(jobs::list_failed(&conn, shoot_id, 200)?)
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_media(state: State<'_, Arc<AppState>>, query: MediaQuery) -> Result<Vec<Media>> {
    let conn = state.db.conn()?;
    Ok(media_repo::query(&conn, &query)?)
}

#[tauri::command]
pub fn get_media(state: State<'_, Arc<AppState>>, media_id: i64) -> Result<Option<Media>> {
    let conn = state.db.conn()?;
    Ok(media_repo::get_by_id(&conn, media_id)?)
}

#[tauri::command]
pub fn media_faces(state: State<'_, Arc<AppState>>, media_id: i64) -> Result<Vec<Face>> {
    let conn = state.db.conn()?;
    Ok(faces::for_media(&conn, media_id)?)
}

/// Stores editor-owned stars and pick/reject flags. This accepts multiple ids
/// so the Sort screen can rate a selection with one keystroke.
#[tauri::command]
pub fn set_media_editorial(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    media_ids: Vec<i64>,
    rating: Option<i64>,
    pick_state: Option<String>,
) -> Result<usize> {
    if media_ids.is_empty() {
        return Err(err("select at least one file to rate"));
    }

    let conn = state.db.conn()?;
    let mut shoot_ids = Vec::new();
    for media_id in &media_ids {
        let media = media_repo::get_by_id(&conn, *media_id)?
            .ok_or_else(|| err(format!("media {media_id} no longer exists")))?;
        shoot_ids.push(media.shoot_id);
    }
    shoot_ids.sort_unstable();
    shoot_ids.dedup();

    let changed = media_repo::set_editorial_state(&conn, &media_ids, rating, pick_state.as_deref())?;
    for shoot_id in shoot_ids {
        events::shoot_changed(&app, shoot_id, "editorial");
    }
    Ok(changed)
}

/// Reveals a file in Explorer or Finder.
#[tauri::command]
pub fn reveal_in_folder(app: AppHandle, path: String) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| err(format!("could not open {path}: {e}")))
}

#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| err(format!("could not open {path}: {e}")))
}

// ---------------------------------------------------------------------------
// Players
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_people(state: State<'_, Arc<AppState>>, shoot_id: Option<i64>) -> Result<Vec<PersonSummary>> {
    let conn = state.db.conn()?;
    Ok(people::list_summaries(&conn, shoot_id)?)
}

#[tauri::command]
pub fn create_person(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    name: String,
    team: Option<String>,
) -> Result<Person> {
    let conn = state.db.conn()?;
    let person = people::get_or_create(&conn, &name, team.as_deref())?;
    logs::record_quiet(
        &conn,
        logs::EVENT_PLAYER_CREATED,
        None,
        None,
        Some(person.id),
        Some(&person.name),
    );
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(person)
}

#[tauri::command]
pub fn rename_person(app: AppHandle, state: State<'_, Arc<AppState>>, person_id: i64, name: String) -> Result<()> {
    let conn = state.db.conn()?;
    people::rename(&conn, person_id, &name)?;
    logs::record_quiet(
        &conn,
        logs::EVENT_PLAYER_RENAMED,
        None,
        None,
        Some(person_id),
        Some(&name),
    );
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(())
}

#[tauri::command]
pub fn update_person(
    state: State<'_, Arc<AppState>>,
    person_id: i64,
    team: Option<String>,
    notes: Option<String>,
) -> Result<()> {
    let conn = state.db.conn()?;
    Ok(people::update(&conn, person_id, team.as_deref(), notes.as_deref())?)
}

/// Folds one player into another (§10, "Merge two people").
#[tauri::command]
pub fn merge_people(app: AppHandle, state: State<'_, Arc<AppState>>, target_id: i64, source_id: i64) -> Result<i64> {
    let moved = state.db.transaction(|conn| people::merge(conn, target_id, source_id))?;
    let conn = state.db.conn()?;
    logs::record_quiet(
        &conn,
        logs::EVENT_PLAYER_MERGED,
        None,
        None,
        Some(target_id),
        Some(&format!("absorbed player {source_id}; {moved} faces now on the target")),
    );
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(moved)
}

#[tauri::command]
pub fn delete_person(app: AppHandle, state: State<'_, Arc<AppState>>, person_id: i64) -> Result<()> {
    let conn = state.db.conn()?;
    people::delete(&conn, person_id)?;
    logs::record_quiet(&conn, logs::EVENT_PLAYER_DELETED, None, None, Some(person_id), None);
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(())
}

/// Drops a player's biometric data but keeps the profile (§22, §24).
#[tauri::command]
pub fn clear_person_recognition(app: AppHandle, state: State<'_, Arc<AppState>>, person_id: i64) -> Result<()> {
    let conn = state.db.conn()?;
    people::clear_recognition_data(&conn, person_id)?;
    logs::record_quiet(
        &conn,
        logs::EVENT_RECOGNITION_DATA_CLEARED,
        None,
        None,
        Some(person_id),
        None,
    );
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(())
}

// ---------------------------------------------------------------------------
// Clusters
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_clusters(
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
    include_named: bool,
) -> Result<Vec<ClusterSummary>> {
    let conn = state.db.conn()?;
    Ok(clusters::list_summaries(&conn, shoot_id, include_named)?)
}

/// Names an unknown cluster, promoting it to a player and adding every face in
/// it to the library (§7).
#[tauri::command]
pub fn name_cluster(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    cluster_id: i64,
    name: String,
    team: Option<String>,
) -> Result<Person> {
    let person = state.db.transaction(|conn| {
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

    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(person)
}

#[tauri::command]
pub fn merge_clusters(app: AppHandle, state: State<'_, Arc<AppState>>, target_id: i64, source_id: i64) -> Result<()> {
    state.db.transaction(|conn| {
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
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(())
}

/// Pulls faces out of a cluster into a new one (§10, "Split incorrect cluster").
#[tauri::command]
pub fn split_cluster(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    cluster_id: i64,
    face_ids: Vec<i64>,
    label: Option<String>,
) -> Result<i64> {
    if face_ids.is_empty() {
        return Err(err("select the faces to split out first"));
    }
    let label = label.unwrap_or_else(|| "Unknown Person (split)".to_string());

    let new_id = state.db.transaction(|conn| {
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

    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(new_id)
}

#[tauri::command]
pub fn ignore_cluster(state: State<'_, Arc<AppState>>, cluster_id: i64) -> Result<()> {
    let conn = state.db.conn()?;
    Ok(clusters::set_status(&conn, cluster_id, ClusterStatus::Ignored)?)
}

// ---------------------------------------------------------------------------
// Albums
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_albums(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<Vec<Album>> {
    let conn = state.db.conn()?;
    Ok(albums::list(&conn, shoot_id)?)
}

#[tauri::command]
pub fn regenerate_albums(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<usize> {
    let created = state.db.transaction(|conn| {
        media_repo::refresh_duplicate_groups(conn, shoot_id, 6)?;
        albums::regenerate(conn, shoot_id)
    })?;
    events::shoot_changed(&app, shoot_id, "albums");
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

#[tauri::command]
pub fn list_groups(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<Vec<Group>> {
    let conn = state.db.conn()?;
    Ok(groups::list(&conn, shoot_id)?)
}

#[tauri::command]
pub fn group_stats(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<GroupStats> {
    let conn = state.db.conn()?;
    let media_total = media_repo::count_for_shoot(&conn, shoot_id)?;
    let ungrouped = groups::ungrouped_count(&conn, shoot_id)?;
    Ok(GroupStats {
        media_total,
        grouped: media_total - ungrouped,
        ungrouped,
    })
}

/// Which groups hold which files, for the chips drawn on each thumbnail.
#[tauri::command]
pub fn group_links(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<Vec<MediaGroupLink>> {
    let conn = state.db.conn()?;
    Ok(groups::links(&conn, shoot_id)?)
}

/// Creates the group the editor just named. The name is what the export folder
/// will be called, which is why it is validated here rather than at export
/// time — a bad name should fail while the person who typed it is looking.
#[tauri::command]
pub fn create_group(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64, name: String) -> Result<Group> {
    let conn = state.db.conn()?;
    let group = groups::get_or_create(&conn, shoot_id, &name, None)?;
    logs::record_quiet(
        &conn,
        logs::EVENT_GROUP_CREATED,
        Some(shoot_id),
        None,
        None,
        Some(&group.name),
    );
    events::shoot_changed(&app, shoot_id, "groups");
    Ok(group)
}

#[tauri::command]
pub fn rename_group(app: AppHandle, state: State<'_, Arc<AppState>>, group_id: i64, name: String) -> Result<Group> {
    let conn = state.db.conn()?;
    groups::rename(&conn, group_id, &name)?;
    let group = groups::get_by_id(&conn, group_id)?.ok_or_else(|| err("that group no longer exists"))?;
    logs::record_quiet(
        &conn,
        logs::EVENT_GROUP_RENAMED,
        Some(group.shoot_id),
        None,
        None,
        Some(&group.name),
    );
    events::shoot_changed(&app, group.shoot_id, "groups");
    Ok(group)
}

/// Sets the on-disk folder name and the note. A blank folder name goes back to
/// using the group's own name.
#[tauri::command]
pub fn update_group(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    group_id: i64,
    folder_name: Option<String>,
    notes: Option<String>,
) -> Result<Group> {
    let conn = state.db.conn()?;
    groups::update(&conn, group_id, folder_name.as_deref(), notes.as_deref())?;
    let group = groups::get_by_id(&conn, group_id)?.ok_or_else(|| err("that group no longer exists"))?;
    events::shoot_changed(&app, group.shoot_id, "groups");
    Ok(group)
}

/// Deletes a group. Only the grouping is lost — no file is touched.
#[tauri::command]
pub fn delete_group(app: AppHandle, state: State<'_, Arc<AppState>>, group_id: i64) -> Result<()> {
    let conn = state.db.conn()?;
    let Some(group) = groups::get_by_id(&conn, group_id)? else {
        return Ok(());
    };
    groups::delete(&conn, group_id)?;
    logs::record_quiet(
        &conn,
        logs::EVENT_GROUP_DELETED,
        Some(group.shoot_id),
        None,
        None,
        Some(&group.name),
    );
    events::shoot_changed(&app, group.shoot_id, "groups");
    Ok(())
}

/// Files the selected media into a group, creating it from `group_name` if the
/// editor typed a new one.
///
/// `move_files` takes them out of every other group in the shoot first — the
/// fix for something filed under the wrong player. Without it a file can
/// legitimately belong to several groups, which is what a clip with two players
/// in it needs.
#[tauri::command]
pub fn add_media_to_group(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
    group_id: Option<i64>,
    group_name: Option<String>,
    media_ids: Vec<i64>,
    move_files: bool,
) -> Result<usize> {
    if media_ids.is_empty() {
        return Err(err("select some files first"));
    }

    let (group, added) = state.db.transaction(|conn| {
        let group = match (group_id, group_name.as_deref()) {
            (Some(id), _) => groups::get_by_id(conn, id)?
                .ok_or_else(|| teo_database::DbError::other("that group no longer exists"))?,
            (None, Some(name)) => groups::get_or_create(conn, shoot_id, name, None)?,
            (None, None) => return Err(teo_database::DbError::other("choose a group or type a new name")),
        };
        let added = if move_files {
            groups::move_media(conn, group.id, &media_ids)?
        } else {
            groups::add_media(conn, group.id, &media_ids)?
        };
        Ok((group, added))
    })?;

    let conn = state.db.conn()?;
    logs::record_quiet(
        &conn,
        logs::EVENT_GROUP_ASSIGNMENT,
        Some(group.shoot_id),
        None,
        None,
        Some(&format!("{} file(s) sorted into {}", media_ids.len(), group.name)),
    );
    events::shoot_changed(&app, group.shoot_id, "groups");
    Ok(added)
}

#[tauri::command]
pub fn remove_media_from_group(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    group_id: i64,
    media_ids: Vec<i64>,
) -> Result<usize> {
    let conn = state.db.conn()?;
    let removed = groups::remove_media(&conn, group_id, &media_ids)?;
    if let Some(group) = groups::get_by_id(&conn, group_id)? {
        events::shoot_changed(&app, group.shoot_id, "groups");
    }
    Ok(removed)
}

#[tauri::command]
pub fn clear_group(app: AppHandle, state: State<'_, Arc<AppState>>, group_id: i64) -> Result<usize> {
    let conn = state.db.conn()?;
    let removed = groups::clear(&conn, group_id)?;
    if let Some(group) = groups::get_by_id(&conn, group_id)? {
        events::shoot_changed(&app, group.shoot_id, "groups");
    }
    Ok(removed)
}

/// The head start: one group per player the AI identified, pre-filled with that
/// player's album, so the editor corrects rather than sorts from scratch.
///
/// Running it again after naming more faces tops the groups up; it never undoes
/// a manual edit.
#[tauri::command]
pub fn groups_from_ai_albums(app: AppHandle, state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<SeedResult> {
    let (groups_touched, files) = state
        .db
        .transaction(|conn| groups::seed_from_player_albums(conn, shoot_id))?;

    let conn = state.db.conn()?;
    logs::record_quiet(
        &conn,
        logs::EVENT_GROUP_ASSIGNMENT,
        Some(shoot_id),
        None,
        None,
        Some(&format!(
            "{groups_touched} group(s) seeded from AI albums with {files} file(s)"
        )),
    );
    events::shoot_changed(&app, shoot_id, "groups");
    Ok(SeedResult {
        groups: groups_touched,
        files,
    })
}

/// Turns one AI album into an editable group — the "this one is right, I will
/// fix the rest by hand" path from the Albums screen.
#[tauri::command]
pub fn group_from_album(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    album_id: i64,
    name: Option<String>,
) -> Result<Group> {
    let group = state.db.transaction(|conn| {
        let album = albums::get_by_id(conn, album_id)?
            .ok_or_else(|| teo_database::DbError::other("that album no longer exists"))?;
        let label = name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&album.name);
        let group = groups::get_or_create(conn, album.shoot_id, label, album.person_ids.first().copied())?;
        groups::add_media(conn, group.id, &albums::media_ids(conn, album_id, None)?)?;
        groups::get_by_id(conn, group.id)?.ok_or_else(|| teo_database::DbError::other("that group no longer exists"))
    })?;

    events::shoot_changed(&app, group.shoot_id, "groups");
    Ok(group)
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_faces(state: State<'_, Arc<AppState>>, query: FaceQuery) -> Result<Vec<FaceWithContext>> {
    let conn = state.db.conn()?;
    Ok(faces::query(&conn, &query)?)
}

/// Accepts the AI's suggestion for these faces.
#[tauri::command]
pub fn confirm_faces(app: AppHandle, state: State<'_, Arc<AppState>>, face_ids: Vec<i64>) -> Result<usize> {
    let updated = state.db.transaction(|conn| {
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
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(updated)
}

/// "Wrong person" — sends the faces back to the unknown pool.
#[tauri::command]
pub fn reject_faces(app: AppHandle, state: State<'_, Arc<AppState>>, face_ids: Vec<i64>) -> Result<usize> {
    let updated = state.db.transaction(|conn| {
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
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(updated)
}

/// Bulk-assigns selected faces to a player (§10). A correction made here
/// becomes a library sample, which is how the user's fix improves future
/// recognition (§6).
#[tauri::command]
pub fn assign_faces(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    face_ids: Vec<i64>,
    person_id: Option<i64>,
    person_name: Option<String>,
) -> Result<usize> {
    if face_ids.is_empty() {
        return Err(err("select at least one face"));
    }

    let updated = state.db.transaction(|conn| {
        let person_id = match (person_id, person_name.as_deref()) {
            (Some(id), _) => id,
            (None, Some(name)) => people::get_or_create(conn, name, None)?.id,
            (None, None) => return Err(teo_database::DbError::other("choose or name a player")),
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

    events::emit(&app, events::LIBRARY_CHANGED, ());
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
#[tauri::command]
pub async fn add_manual_face(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    media_id: i64,
    bbox: BoundingBox,
    frame_time: Option<f64>,
) -> Result<ManualFaceResult> {
    let bbox = validate_manual_bbox(bbox)?;
    let state = Arc::clone(&state);
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<ManualFaceResult> {
        let media = {
            let conn = state.db.conn()?;
            media_repo::get_by_id(&conn, media_id)?.ok_or_else(|| err("that file is no longer in the library"))?
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

        let settings = state.settings();
        let mut engine = crate::pipeline::Engine::new(&state.paths, &settings)?;
        let (embedding, quality) = engine.embed_manual_face(&media, bbox, frame_time)?;

        let (library, used_people) = {
            let conn = state.db.conn()?;
            let used_people: Vec<i64> = faces::for_media(&conn, media.id)?
                .into_iter()
                .filter(|face| face.assignment != FaceAssignment::Ignored.as_str())
                .filter(|face| match frame_time {
                    Some(timestamp) => face.frame_time.is_some_and(|at| (at - timestamp).abs() < 0.01),
                    None => true,
                })
                .filter_map(|face| face.person_id)
                .collect();
            (faces::library_vectors(&conn)?, used_people)
        };
        let matcher = FaceMatcher::build(library.into_iter().filter_map(|sample| {
            sample
                .person_id
                .filter(|person_id| !used_people.contains(person_id))
                .map(|person_id| (person_id, sample.embedding))
        }));
        let matched = matcher.match_one(&embedding, &settings.matcher_config());

        let result = state.db.transaction(|conn| {
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
                .ok_or_else(|| teo_database::DbError::other("the new face could not be read back"))?;
            Ok(ManualFaceResult { face, suggested_person })
        })?;
        Ok(result)
    })
    .await
    .map_err(|e| err(format!("manual face recognition stopped unexpectedly: {e}")))??;

    events::emit(&app, events::LIBRARY_CHANGED, ());
    events::shoot_changed(&app, result.face.shoot_id, "manualFace");
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
}

#[tauri::command]
pub async fn name_face(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    face_id: i64,
    name: String,
    team: Option<String>,
) -> Result<NameFaceResult> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(err("give the person a name"));
    }

    let state = Arc::clone(&state);
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<NameFaceResult> {
        let (person, faces_named, shoot_id, appearances_before_matching) = state.db.transaction(|conn| {
            let face = faces::get_by_id(conn, face_id)?
                .ok_or_else(|| teo_database::DbError::other("that face is no longer in the library"))?;
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
            let appearances_before_matching = conn.query_row(
                "SELECT COUNT(*) FROM faces
                  WHERE shoot_id = ?1 AND person_id = ?2
                    AND assignment IN ('suggested','confirmed')",
                teo_database::rusqlite::params![face.shoot_id, person.id],
                |row| row.get::<_, i64>(0),
            )?;
            Ok((person, faces_named, face.shoot_id, appearances_before_matching))
        })?;

        // The newly confirmed face is now a reference sample. Recognition may
        // have completed before the reviewer named it, so compare the remaining
        // unknown faces immediately. This is deliberately format-agnostic:
        // camera RAW, JPEG, PNG, HEIC and TIFF are all photo rows here.
        stages::recognise_shoot(&state.db, shoot_id, &state.settings())?;

        state
            .db
            .transaction(|conn| {
                let appearances_after_matching = conn.query_row(
                    "SELECT COUNT(*) FROM faces
                  WHERE shoot_id = ?1 AND person_id = ?2
                    AND assignment IN ('suggested','confirmed')",
                    teo_database::rusqlite::params![shoot_id, person.id],
                    |row| row.get::<_, i64>(0),
                )?;
                let matches_found = appearances_after_matching
                    .saturating_sub(appearances_before_matching)
                    .max(0) as usize;
                albums::regenerate(conn, shoot_id)?;
                let player_album = albums::list(conn, shoot_id)?.into_iter().find(|album| {
                    album.album_type == AlbumType::Player.as_str() && album.person_ids.contains(&person.id)
                });

                let group = groups::get_or_create(conn, shoot_id, &person.name, Some(person.id))?;
                let files_added = match player_album {
                    Some(album) => groups::add_media(conn, group.id, &albums::media_ids(conn, album.id, None)?)?,
                    None => 0,
                };
                let group = groups::get_by_id(conn, group.id)?
                    .ok_or_else(|| teo_database::DbError::other("that group no longer exists"))?;

                Ok(NameFaceResult {
                    person,
                    faces_named,
                    matches_found,
                    group,
                    files_added,
                })
            })
            .map_err(CommandError::from)
    })
    .await
    .map_err(|error| err(format!("face matching stopped unexpectedly: {error}")))??;

    events::emit(&app, events::LIBRARY_CHANGED, ());
    events::shoot_changed(&app, result.group.shoot_id, "groups");
    Ok(result)
}

/// "Remove false face detection" — keeps the row but takes it out of every
/// count and album.
#[tauri::command]
pub fn ignore_faces(state: State<'_, Arc<AppState>>, face_ids: Vec<i64>) -> Result<usize> {
    let updated = state.db.transaction(|conn| {
        let updated = faces::ignore_many(conn, &face_ids)?;
        video::delete_for_faces(conn, &face_ids)?;
        Ok(updated)
    })?;

    // Face counts on the affected images are now stale.
    let conn = state.db.conn()?;
    for face_id in &face_ids {
        if let Some(face) = faces::get_by_id(&conn, *face_id)? {
            media_repo::refresh_face_count(&conn, face.media_id)?;
        }
    }
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn video_timelines(state: State<'_, Arc<AppState>>, media_id: i64) -> Result<Vec<VideoTimeline>> {
    let conn = state.db.conn()?;
    Ok(video::timelines(&conn, media_id)?)
}

#[tauri::command]
pub fn video_sample_frames(state: State<'_, Arc<AppState>>, media_id: i64) -> Result<Vec<f64>> {
    let conn = state.db.conn()?;
    let stored = video::sample_times(&conn, media_id)?;
    let duration = media_repo::get_by_id(&conn, media_id)?.and_then(|media| {
        (media.media_type == MediaType::Video.as_str())
            .then_some(media.duration)
            .flatten()
    });
    Ok(review_sample_times(stored, duration, &state.settings().video_config()))
}

fn review_sample_times(
    stored: Vec<f64>,
    duration: Option<f64>,
    config: &teo_video_analysis::VideoAnalysisConfig,
) -> Vec<f64> {
    // The interval plan is inexpensive and deterministic, so existing videos
    // analysed before sample-frame indexing still expose every cadence frame.
    // Persisted scene-change samples are unioned in when they exist.
    let mut milliseconds = std::collections::BTreeSet::new();
    for timestamp in stored.into_iter().chain(
        teo_video_analysis::plan_frames(duration, &[], config)
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

#[tauri::command]
pub fn preview_export(
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
    destination: String,
    options: ExportOptions,
) -> Result<ExportPreview> {
    let plan = crate::export::preview(&state.db, shoot_id, std::path::Path::new(&destination), &options)?;
    Ok(ExportPreview {
        file_count: plan.len(),
        total_bytes: plan.total_bytes(),
        folders: plan.folders.clone(),
    })
}

#[tauri::command]
pub fn start_export(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
    destination: String,
    options: ExportOptions,
) -> Result<i64> {
    Ok(crate::export::start(
        app,
        Arc::clone(&state),
        shoot_id,
        PathBuf::from(destination),
        options,
    )?)
}

#[tauri::command]
pub fn cancel_export(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<()> {
    state.cancel_shoot(shoot_id);
    Ok(())
}

#[tauri::command]
pub fn list_exports(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<Vec<ExportRecord>> {
    let conn = state.db.conn()?;
    Ok(exports::list(&conn, shoot_id, 20)?)
}

// ---------------------------------------------------------------------------
// Logs and privacy (§24, §25)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn recent_logs(state: State<'_, Arc<AppState>>, shoot_id: Option<i64>, limit: i64) -> Result<Vec<LogEntry>> {
    let conn = state.db.conn()?;
    Ok(logs::recent(&conn, shoot_id, limit)?)
}

/// Deletes every embedding while leaving detections and albums intact.
#[tauri::command]
pub fn clear_all_embeddings(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<usize> {
    let cleared = state.db.transaction(faces::clear_all_embeddings)?;
    let conn = state.db.conn()?;
    logs::record_quiet(
        &conn,
        logs::EVENT_RECOGNITION_DATA_CLEARED,
        None,
        None,
        None,
        Some(&format!("{cleared} embedding(s) deleted")),
    );
    events::notice(&app, "success", format!("Deleted {cleared} face embeddings."));
    Ok(cleared)
}

/// The full reset: every face, cluster and player profile in the database.
#[tauri::command]
pub fn clear_all_recognition_data(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<()> {
    state.db.transaction(|conn| {
        conn.execute("DELETE FROM video_detections", [])?;
        conn.execute("DELETE FROM faces", [])?;
        conn.execute("DELETE FROM clusters", [])?;
        conn.execute("DELETE FROM albums", [])?;
        conn.execute("DELETE FROM people", [])?;
        conn.execute("UPDATE media SET face_count = 0, processing_status = 'indexed'", [])?;
        Ok(())
    })?;

    let conn = state.db.conn()?;
    logs::record_quiet(
        &conn,
        logs::EVENT_RECOGNITION_DATA_CLEARED,
        None,
        None,
        None,
        Some("all"),
    );
    events::notice(&app, "success", "All recognition data has been deleted.");
    Ok(())
}

#[tauri::command]
pub fn clear_thumbnail_cache(state: State<'_, Arc<AppState>>) -> Result<u64> {
    let removed = state.thumbnails.clear()? + state.proxies.clear()?;
    let conn = state.db.conn()?;
    conn.execute("UPDATE media SET thumbnail_path = NULL", [])
        .map_err(teo_database::DbError::from)?;
    Ok(removed)
}

#[tauri::command]
pub fn clear_log(state: State<'_, Arc<AppState>>) -> Result<()> {
    let conn = state.db.conn()?;
    Ok(logs::clear(&conn)?)
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
        let config = teo_video_analysis::VideoAnalysisConfig {
            sample_interval: 5.0,
            max_frames: 60,
            ..Default::default()
        };
        let samples = review_sample_times(vec![2.2, 10.0], Some(16.0), &config);

        assert_eq!(samples, vec![0.0, 2.2, 5.0, 10.0, 15.0]);
    }
}
