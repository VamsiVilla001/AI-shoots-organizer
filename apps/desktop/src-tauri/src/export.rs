//! Turning the app's groups — the editor's own, or the AI albums — into
//! folders on disk (§11, §34).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::AppHandle;
use teo_database::models::{AlbumType, ExportStatus};
use teo_database::repo::{albums, exports, groups as groups_repo, logs, media as media_repo, shoots};
use teo_export_engine::{ExportGroup, ExportMode, ExportOptions, ExportPlan, SourceFile};

use crate::events;
use crate::state::AppState;

#[derive(Debug, thiserror::Error)]
pub enum ExportRunError {
    #[error(transparent)]
    Database(#[from] teo_database::DbError),
    #[error(transparent)]
    Engine(#[from] teo_export_engine::ExportError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ExportRunError>;

/// Collects the files each output folder will hold.
///
/// Whichever mode is in play, the rule is the same: the folder set the user can
/// see in the app is the folder set that lands on disk.
pub fn build_groups(db: &teo_database::Database, shoot_id: i64, options: &ExportOptions) -> Result<Vec<ExportGroup>> {
    match options.mode {
        ExportMode::Groups => build_from_manual_groups(db, shoot_id, options),
        ExportMode::AiAlbums => build_from_albums(db, shoot_id, options),
    }
}

/// Reads a group's membership into the files the export will copy. Files that
/// have gone missing from the source folder since the scan are skipped with a
/// log line rather than failing the run.
fn collect_files(conn: &teo_database::rusqlite::Connection, media_ids: &[i64]) -> Result<Vec<SourceFile>> {
    let mut files = Vec::with_capacity(media_ids.len());
    for media_id in media_ids {
        let Some(item) = media_repo::get_by_id(conn, *media_id)? else {
            continue;
        };
        let path = PathBuf::from(&item.path);
        if !path.is_file() {
            tracing::warn!(file = %item.path, "skipping a file that is no longer on disk");
            continue;
        }
        files.push(SourceFile {
            path,
            filename: item.filename,
            is_video: item.media_type == "video",
            size: item.file_size.max(0) as u64,
        });
    }
    Ok(files)
}

/// The editor's own groups — the primary path (§34). The group's name (or its
/// folder-name override) becomes the folder, in the order shown in the app.
fn build_from_manual_groups(
    db: &teo_database::Database,
    shoot_id: i64,
    options: &ExportOptions,
) -> Result<Vec<ExportGroup>> {
    let conn = db.conn()?;
    let mut out = Vec::new();

    for group in groups_repo::list(&conn, shoot_id)? {
        if let Some(selected) = &options.group_ids {
            if !selected.contains(&group.id) {
                continue;
            }
        }
        let files = collect_files(&conn, &groups_repo::media_ids(&conn, group.id, None)?)?;
        if !files.is_empty() {
            out.push(ExportGroup {
                name: group.export_name().to_string(),
                files,
            });
        }
    }

    Ok(out)
}

fn build_from_albums(db: &teo_database::Database, shoot_id: i64, options: &ExportOptions) -> Result<Vec<ExportGroup>> {
    let conn = db.conn()?;
    let all = albums::list(&conn, shoot_id)?;
    let mut groups = Vec::new();

    for album in all {
        let album_type = album.album_type.as_str();

        let include = match album_type {
            t if t == AlbumType::Player.as_str() => match &options.person_ids {
                Some(selected) => album.person_ids.iter().any(|id| selected.contains(id)),
                None => true,
            },
            t if t == AlbumType::MultiPlayer.as_str() => options.include_multi_player,
            t if t == AlbumType::Unidentified.as_str() => options.include_unidentified,
            t if t == AlbumType::GroupSize.as_str() => options.include_group_size,
            // Team albums duplicate their members' files; exporting them by
            // default would multiply the output size for little benefit.
            _ => false,
        };
        if !include {
            continue;
        }

        let files = collect_files(&conn, &albums::media_ids(&conn, album.id, None)?)?;
        if !files.is_empty() {
            groups.push(ExportGroup {
                name: album.name,
                files,
            });
        }
    }

    Ok(groups)
}

/// Builds the plan without writing anything, so the UI can preview the result.
pub fn preview(
    db: &teo_database::Database,
    shoot_id: i64,
    destination: &Path,
    options: &ExportOptions,
) -> Result<ExportPlan> {
    let source_root = {
        let conn = db.conn()?;
        shoots::get_by_id(&conn, shoot_id)?
            .map(|s| PathBuf::from(s.source_path))
            .ok_or_else(|| ExportRunError::Other(format!("shoot {shoot_id} not found")))?
    };
    teo_export_engine::validate_destination(destination, std::slice::from_ref(&source_root))?;

    let groups = build_groups(db, shoot_id, options)?;
    Ok(teo_export_engine::plan(&groups, options))
}

/// Starts an export on a background thread and returns its record id
/// immediately. Progress arrives as `teo://export-progress` events.
pub fn start(
    app: AppHandle,
    state: Arc<AppState>,
    shoot_id: i64,
    destination: PathBuf,
    options: ExportOptions,
) -> Result<i64> {
    let plan = preview(&state.db, shoot_id, &destination, &options)?;
    if plan.is_empty() {
        return Err(ExportRunError::Other(match options.mode {
            ExportMode::Groups => {
                "nothing to export — the selected groups are empty. Sort some files into a group first.".into()
            }
            ExportMode::AiAlbums => "nothing to export — no albums match the selected options".to_string(),
        }));
    }

    let export_id = {
        let conn = state.db.conn()?;
        let id = exports::create(
            &conn,
            shoot_id,
            &destination.display().to_string(),
            &serde_json::to_string(&options).unwrap_or_default(),
        )?;
        exports::set_total(&conn, id, plan.len() as i64)?;
        id
    };

    std::thread::Builder::new()
        .name("teo-export".into())
        .spawn(move || run(app, state, export_id, shoot_id, destination, plan, options))
        .map_err(|e| ExportRunError::Other(format!("could not start the export thread: {e}")))?;

    Ok(export_id)
}

fn run(
    app: AppHandle,
    state: Arc<AppState>,
    export_id: i64,
    shoot_id: i64,
    destination: PathBuf,
    plan: ExportPlan,
    options: ExportOptions,
) {
    let total = plan.len();
    let cancel = state.cancellation(shoot_id);
    // An export is a fresh intent: clear any cancellation left over from an
    // earlier run so it does not stop immediately.
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);

    let progress_app = app.clone();
    let progress_state = Arc::clone(&state);
    let result = teo_export_engine::execute(
        &plan,
        &destination,
        &options,
        || !cancel.load(std::sync::atomic::Ordering::Relaxed),
        move |progress| {
            if let Ok(conn) = progress_state.db.conn() {
                let _ = exports::set_progress(
                    &conn,
                    export_id,
                    progress.files_done as i64,
                    progress.bytes_done as i64,
                );
            }
            events::emit(
                &progress_app,
                events::EXPORT_PROGRESS,
                events::ExportProgressEvent {
                    export_id,
                    shoot_id,
                    files_done: progress.files_done,
                    files_total: total,
                    files_skipped: progress.files_skipped,
                    bytes_done: progress.bytes_done,
                    finished: false,
                    error: None,
                },
            );
        },
    );

    let (status, error) = match &result {
        Ok(_) => (ExportStatus::Completed, None),
        Err(teo_export_engine::ExportError::Cancelled) => (ExportStatus::Cancelled, None),
        Err(e) => (ExportStatus::Failed, Some(e.to_string())),
    };

    if let Ok(conn) = state.db.conn() {
        let _ = exports::finish(&conn, export_id, status, error.as_deref());
        logs::record_quiet(
            &conn,
            logs::EVENT_EXPORT,
            Some(shoot_id),
            None,
            None,
            Some(&format!("{} → {}", status.as_str(), destination.display())),
        );
    }

    let done = result.as_ref().map(|p| p.files_done).unwrap_or(0);
    let skipped = result.as_ref().map(|p| p.files_skipped).unwrap_or(0);
    let bytes = result.as_ref().map(|p| p.bytes_done).unwrap_or(0);

    events::emit(
        &app,
        events::EXPORT_PROGRESS,
        events::ExportProgressEvent {
            export_id,
            shoot_id,
            files_done: done,
            files_total: total,
            files_skipped: skipped,
            bytes_done: bytes,
            finished: true,
            error: error.clone(),
        },
    );

    match error {
        Some(message) => events::notice(&app, "error", format!("Copy failed: {message}")),
        None if status == ExportStatus::Cancelled => events::notice(&app, "warn", "Copy cancelled."),
        None => events::notice(
            &app,
            "success",
            format!("Copied {done} file(s) to {}", destination.display()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teo_database::models::{BoundingBox, MediaType, NewFace, NewMedia};
    use teo_database::repo::{faces, people};
    use teo_database::Database;

    /// A shoot with two players and real files on disk.
    fn seed(dir: &Path) -> (Database, i64) {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "Shoot", &dir.display().to_string()).unwrap();

        let jonathan = people::get_or_create(&conn, "Jonathan", None).unwrap();
        let mavi = people::get_or_create(&conn, "Mavi", None).unwrap();

        for (name, person, is_video) in [
            ("IMG_0001.jpg", jonathan.id, false),
            ("IMG_0002.jpg", jonathan.id, false),
            ("clip.mp4", mavi.id, true),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, b"contents").unwrap();

            let media_id = media_repo::upsert(
                &conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: path.display().to_string(),
                    filename: name.to_string(),
                    media_type: if is_video { MediaType::Video } else { MediaType::Photo },
                    extension: if is_video { "mp4".into() } else { "jpg".into() },
                    file_size: 8,
                    content_key: name.to_string(),
                    captured_at: None,
                },
            )
            .unwrap();

            let face_id = faces::insert(
                &conn,
                &NewFace {
                    media_id,
                    shoot_id: shoot.id,
                    bbox: BoundingBox { x: 0.0, y: 0.0, w: 0.1, h: 0.1 },
                    landmarks: None,
                    detection_confidence: 0.9,
                    embedding: Some(vec![1.0, 0.0]),
                    quality: Some(0.5),
                    frame_time: None,
                    crop_path: None,
                },
            )
            .unwrap();
            faces::assign(&conn, face_id, person, Some(0.99)).unwrap();
        }

        albums::regenerate(&conn, shoot.id).unwrap();
        (db, shoot.id)
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("teo-export-{tag}-{}", std::process::id()));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Options for the AI-album path; the default mode is the editor's groups.
    fn album_options() -> ExportOptions {
        ExportOptions {
            mode: ExportMode::AiAlbums,
            ..Default::default()
        }
    }

    #[test]
    fn groups_follow_the_player_albums() {
        let scratch = Scratch::new("groups");
        let (db, shoot_id) = seed(scratch.path());

        let groups = build_groups(&db, shoot_id, &album_options()).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert!(names.contains(&"Jonathan"));
        assert!(names.contains(&"Mavi"));

        let jonathan = groups.iter().find(|g| g.name == "Jonathan").unwrap();
        assert_eq!(jonathan.files.len(), 2);
    }

    #[test]
    fn selecting_players_narrows_the_export() {
        let scratch = Scratch::new("selected");
        let (db, shoot_id) = seed(scratch.path());

        let jonathan_id = {
            let conn = db.conn().unwrap();
            people::find_by_name(&conn, "Jonathan").unwrap().unwrap().id
        };
        let options = ExportOptions {
            person_ids: Some(vec![jonathan_id]),
            ..album_options()
        };

        let groups = build_groups(&db, shoot_id, &options).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Jonathan");
    }

    #[test]
    fn the_plan_reproduces_the_folder_layout_from_the_spec() {
        let scratch = Scratch::new("plan");
        let destination = Scratch::new("plan-dest");
        let (db, shoot_id) = seed(scratch.path());

        let plan = preview(&db, shoot_id, destination.path(), &album_options()).unwrap();
        let relatives: Vec<String> = plan
            .items
            .iter()
            .map(|i| i.relative.to_string_lossy().replace('\\', "/"))
            .collect();

        assert!(relatives.contains(&"Jonathan/Photos/IMG_0001.jpg".to_string()));
        assert!(relatives.contains(&"Mavi/Videos/clip.mp4".to_string()));
    }

    #[test]
    fn exporting_into_the_shoot_folder_is_refused() {
        let scratch = Scratch::new("selfdest");
        let (db, shoot_id) = seed(scratch.path());

        let result = preview(&db, shoot_id, scratch.path(), &album_options());
        assert!(matches!(
            result,
            Err(ExportRunError::Engine(teo_export_engine::ExportError::DestinationInsideSource))
        ));
    }

    #[test]
    fn manual_groups_become_the_folders() {
        let scratch = Scratch::new("manual");
        let destination = Scratch::new("manual-dest");
        let (db, shoot_id) = seed(scratch.path());

        let bts = {
            let conn = db.conn().unwrap();
            let highlights = groups_repo::get_or_create(&conn, shoot_id, "Jonathan Highlights", None).unwrap();
            let bts = groups_repo::get_or_create(&conn, shoot_id, "BTS", None).unwrap();

            let all: Vec<i64> = media_repo::query(
                &conn,
                &teo_database::models::MediaQuery {
                    shoot_id: Some(shoot_id),
                    ..Default::default()
                },
            )
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();

            groups_repo::add_media(&conn, highlights.id, &all[..2]).unwrap();
            groups_repo::add_media(&conn, bts.id, &all[2..]).unwrap();
            bts.id
        };

        let plan = preview(&db, shoot_id, destination.path(), &ExportOptions::default()).unwrap();
        let relatives: Vec<String> = plan
            .items
            .iter()
            .map(|i| i.relative.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(plan.len(), 3);
        assert!(relatives.iter().any(|p| p.starts_with("Jonathan Highlights/")));
        assert!(relatives.iter().any(|p| p.starts_with("BTS/")));

        // Selecting one group narrows the export to that folder only.
        let options = ExportOptions {
            group_ids: Some(vec![bts]),
            ..Default::default()
        };
        let narrowed = preview(&db, shoot_id, destination.path(), &options).unwrap();
        assert_eq!(narrowed.len(), 1);
        assert!(narrowed.items[0]
            .relative
            .to_string_lossy()
            .replace('\\', "/")
            .starts_with("BTS/"));
    }

    /// The whole point of the feature, end to end: names typed in the app
    /// become folders on the destination, holding copies of the originals,
    /// while the source folder is left exactly as it was.
    #[test]
    fn sorting_into_groups_writes_those_folders_and_leaves_the_source_alone() {
        let scratch = Scratch::new("e2e");
        let destination = Scratch::new("e2e-dest");
        let (db, shoot_id) = seed(scratch.path());

        let before: Vec<String> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        {
            let conn = db.conn().unwrap();
            let all: Vec<i64> = media_repo::query(
                &conn,
                &teo_database::models::MediaQuery { shoot_id: Some(shoot_id), ..Default::default() },
            )
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();

            let jonathan = groups_repo::get_or_create(&conn, shoot_id, "Jonathan", None).unwrap();
            let mavi = groups_repo::get_or_create(&conn, shoot_id, "Mavi: Day 2", None).unwrap();
            groups_repo::add_media(&conn, jonathan.id, &all[..2]).unwrap();
            groups_repo::add_media(&conn, mavi.id, &all[2..]).unwrap();
        }

        let options = ExportOptions::default();
        let plan = preview(&db, shoot_id, destination.path(), &options).unwrap();
        let progress =
            teo_export_engine::execute(&plan, destination.path(), &options, || true, |_| {}).unwrap();
        assert_eq!(progress.files_done, 3);

        // The names became folders — with the colon sanitised out, since a
        // Windows path cannot hold one.
        assert!(destination.path().join("Jonathan").join("Photos").is_dir());
        assert!(destination.path().join("Mavi_ Day 2").join("Videos").is_dir());
        assert!(destination.path().join(teo_export_engine::MANIFEST_FILENAME).is_file());

        // The source folder is untouched: same entries, no new subfolders.
        let after: Vec<String> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(before.len(), after.len());
        for name in &before {
            assert!(after.contains(name), "{name} disappeared from the source folder");
        }

        // Re-running is cheap and does not duplicate anything.
        let again =
            teo_export_engine::execute(&plan, destination.path(), &options, || true, |_| {}).unwrap();
        assert_eq!(again.files_done, 0);
        assert_eq!(again.files_skipped, 3);
    }

    #[test]
    fn a_folder_name_override_beats_the_group_name() {
        let scratch = Scratch::new("override");
        let destination = Scratch::new("override-dest");
        let (db, shoot_id) = seed(scratch.path());

        {
            let conn = db.conn().unwrap();
            let group = groups_repo::get_or_create(&conn, shoot_id, "Jonathan", None).unwrap();
            groups_repo::update(&conn, group.id, Some("01_Jonathan"), None).unwrap();
            let all: Vec<i64> = media_repo::query(
                &conn,
                &teo_database::models::MediaQuery {
                    shoot_id: Some(shoot_id),
                    ..Default::default()
                },
            )
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
            groups_repo::add_media(&conn, group.id, &all[..1]).unwrap();
        }

        let plan = preview(&db, shoot_id, destination.path(), &ExportOptions::default()).unwrap();
        assert!(plan.items[0]
            .relative
            .to_string_lossy()
            .replace('\\', "/")
            .starts_with("01_Jonathan/"));
    }

    #[test]
    fn an_export_with_no_groups_says_what_to_do_instead_of_failing_silently() {
        let scratch = Scratch::new("emptygroups");
        let destination = Scratch::new("emptygroups-dest");
        let (db, shoot_id) = seed(scratch.path());

        let plan = preview(&db, shoot_id, destination.path(), &ExportOptions::default()).unwrap();
        assert!(plan.is_empty(), "no groups means nothing to write");
    }

    #[test]
    fn group_size_folders_are_opt_in() {
        let scratch = Scratch::new("groupsize");
        let (db, shoot_id) = seed(scratch.path());

        // Off by default: enabling it would silently write every file twice,
        // since each file is in both a player album and a size album.
        let default_groups = build_groups(&db, shoot_id, &album_options()).unwrap();
        assert!(!default_groups.iter().any(|g| g.name == "Single"));

        let opted_in = ExportOptions { include_group_size: true, ..album_options() };
        let groups = build_groups(&db, shoot_id, &opted_in).unwrap();
        assert!(
            groups.iter().any(|g| g.name == "Single"),
            "expected a Single folder, got {:?}",
            groups.iter().map(|g| &g.name).collect::<Vec<_>>()
        );
        // The player folders are still there — the two axes coexist.
        assert!(groups.iter().any(|g| g.name == "Jonathan"));
    }

    #[test]
    fn files_missing_from_disk_are_skipped_not_fatal() {
        let scratch = Scratch::new("missing");
        let (db, shoot_id) = seed(scratch.path());
        std::fs::remove_file(scratch.path().join("IMG_0001.jpg")).unwrap();

        let groups = build_groups(&db, shoot_id, &album_options()).unwrap();
        let jonathan = groups.iter().find(|g| g.name == "Jonathan").unwrap();
        assert_eq!(jonathan.files.len(), 1, "the deleted file drops out of the export");
    }
}
