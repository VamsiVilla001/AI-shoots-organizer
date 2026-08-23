//! The Tauri IPC surface — everything the React frontend can ask for.
//!
//! Commands stay thin: validate, call into a crate, return data. Anything that
//! could take longer than a frame is queued as a job or spawned onto a thread
//! rather than run here, because a command blocks the caller's promise.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

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
    CommandError { message: message.into() }
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
    let registry = ModelRegistry::new(&state.paths.models);

    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        paths: state.paths.clone(),
        media_url_base: state.media_url_base.clone(),
        ffmpeg_available: ffmpeg.is_some(),
        ffmpeg_version: ffmpeg.as_ref().and_then(|f| f.version()),
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
    logs::record_quiet(&conn, logs::EVENT_PLAYER_CREATED, None, None, Some(person.id), Some(&person.name));
    events::emit(&app, events::LIBRARY_CHANGED, ());
    Ok(person)
}

#[tauri::command]
pub fn rename_person(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    person_id: i64,
    name: String,
) -> Result<()> {
    let conn = state.db.conn()?;
    people::rename(&conn, person_id, &name)?;
    logs::record_quiet(&conn, logs::EVENT_PLAYER_RENAMED, None, None, Some(person_id), Some(&name));
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
pub fn merge_people(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    target_id: i64,
    source_id: i64,
) -> Result<i64> {
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
    logs::record_quiet(&conn, logs::EVENT_RECOGNITION_DATA_CLEARED, None, None, Some(person_id), None);
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
pub fn merge_clusters(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    target_id: i64,
    source_id: i64,
) -> Result<()> {
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
    let created = state.db.transaction(|conn| albums::regenerate(conn, shoot_id))?;
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
    Ok(GroupStats { media_total, grouped: media_total - ungrouped, ungrouped })
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
pub fn create_group(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
    name: String,
) -> Result<Group> {
    let conn = state.db.conn()?;
    let group = groups::get_or_create(&conn, shoot_id, &name, None)?;
    logs::record_quiet(&conn, logs::EVENT_GROUP_CREATED, Some(shoot_id), None, None, Some(&group.name));
    events::shoot_changed(&app, shoot_id, "groups");
    Ok(group)
}

#[tauri::command]
pub fn rename_group(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    group_id: i64,
    name: String,
) -> Result<Group> {
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
            (None, None) => {
                return Err(teo_database::DbError::other("choose a group or type a new name"))
            }
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
pub fn groups_from_ai_albums(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    shoot_id: i64,
) -> Result<SeedResult> {
    let (groups_touched, files) =
        state.db.transaction(|conn| groups::seed_from_player_albums(conn, shoot_id))?;

    let conn = state.db.conn()?;
    logs::record_quiet(
        &conn,
        logs::EVENT_GROUP_ASSIGNMENT,
        Some(shoot_id),
        None,
        None,
        Some(&format!("{groups_touched} group(s) seeded from AI albums with {files} file(s)")),
    );
    events::shoot_changed(&app, shoot_id, "groups");
    Ok(SeedResult { groups: groups_touched, files })
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
        let label = name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&album.name);
        let group = groups::get_or_create(conn, album.shoot_id, label, album.person_ids.first().copied())?;
        groups::add_media(conn, group.id, &albums::media_ids(conn, album_id, None)?)?;
        groups::get_by_id(conn, group.id)?
            .ok_or_else(|| teo_database::DbError::other("that group no longer exists"))
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

/// "Remove false face detection" — keeps the row but takes it out of every
/// count and album.
#[tauri::command]
pub fn ignore_faces(state: State<'_, Arc<AppState>>, face_ids: Vec<i64>) -> Result<usize> {
    let updated = state.db.transaction(|conn| faces::ignore_many(conn, &face_ids))?;

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
    logs::record_quiet(&conn, logs::EVENT_RECOGNITION_DATA_CLEARED, None, None, None, Some("all"));
    events::notice(&app, "success", "All recognition data has been deleted.");
    Ok(())
}

#[tauri::command]
pub fn clear_thumbnail_cache(state: State<'_, Arc<AppState>>) -> Result<u64> {
    let removed = state.thumbnails.clear()?;
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
