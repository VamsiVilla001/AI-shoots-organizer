//! Turning albums into folders on disk (§11).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::AppHandle;
use teo_database::models::{AlbumType, ExportStatus};
use teo_database::repo::{albums, exports, logs, media as media_repo, shoots};
use teo_export_engine::{ExportGroup, ExportOptions, ExportPlan, SourceFile};

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
/// Album membership is the source of truth, so what gets exported is exactly
/// what the user reviewed on the Albums screen.
pub fn build_groups(
    db: &teo_database::Database,
    shoot_id: i64,
    options: &ExportOptions,
) -> Result<Vec<ExportGroup>> {
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

        let media_ids = albums::media_ids(&conn, album.id, None)?;
        let mut files = Vec::with_capacity(media_ids.len());
        for media_id in media_ids {
            let Some(item) = media_repo::get_by_id(&conn, media_id)? else { continue };
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

        if !files.is_empty() {
            groups.push(ExportGroup { name: album.name, files });
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
        return Err(ExportRunError::Other(
            "nothing to export — no albums match the selected options".into(),
        ));
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

    #[test]
    fn groups_follow_the_player_albums() {
        let scratch = Scratch::new("groups");
        let (db, shoot_id) = seed(scratch.path());

        let groups = build_groups(&db, shoot_id, &ExportOptions::default()).unwrap();
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
        let options = ExportOptions { person_ids: Some(vec![jonathan_id]), ..Default::default() };

        let groups = build_groups(&db, shoot_id, &options).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Jonathan");
    }

    #[test]
    fn the_plan_reproduces_the_folder_layout_from_the_spec() {
        let scratch = Scratch::new("plan");
        let destination = Scratch::new("plan-dest");
        let (db, shoot_id) = seed(scratch.path());

        let plan = preview(&db, shoot_id, destination.path(), &ExportOptions::default()).unwrap();
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

        let result = preview(&db, shoot_id, scratch.path(), &ExportOptions::default());
        assert!(matches!(
            result,
            Err(ExportRunError::Engine(teo_export_engine::ExportError::DestinationInsideSource))
        ));
    }

    #[test]
    fn group_size_folders_are_opt_in() {
        let scratch = Scratch::new("groupsize");
        let (db, shoot_id) = seed(scratch.path());

        // Off by default: enabling it would silently write every file twice,
        // since each file is in both a player album and a size album.
        let default_groups = build_groups(&db, shoot_id, &ExportOptions::default()).unwrap();
        assert!(!default_groups.iter().any(|g| g.name == "Single"));

        let opted_in = ExportOptions { include_group_size: true, ..Default::default() };
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

        let groups = build_groups(&db, shoot_id, &ExportOptions::default()).unwrap();
        let jonathan = groups.iter().find(|g| g.name == "Jonathan").unwrap();
        assert_eq!(jonathan.files.len(), 1, "the deleted file drops out of the export");
    }
}
