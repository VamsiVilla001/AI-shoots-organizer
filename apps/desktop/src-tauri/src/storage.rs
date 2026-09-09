use crate::{commands::Result, state::AppState};
use serde::Serialize;
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootStorage {
    record_bytes: u64,
    preview_bytes: u64,
}

/// Kept separate from frequent progress events: file metadata can be slow.
#[tauri::command]
pub async fn get_shoot_storage(state: State<'_, Arc<AppState>>, shoot_id: i64) -> Result<ShootStorage> {
    let state = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || -> Result<ShootStorage> {
        let (record_bytes, paths) = {
            let conn = state.db.conn()?;
            let record_bytes = skwad_database::repo::storage::shoot_record_bytes(&conn, shoot_id)?;
            let mut paths = HashSet::<PathBuf>::new();
            let mut query = conn.prepare("SELECT thumbnail_path FROM media WHERE shoot_id = ?1 AND thumbnail_path IS NOT NULL UNION SELECT crop_path FROM faces WHERE shoot_id = ?1 AND crop_path IS NOT NULL")?;
            for path in query.query_map([shoot_id], |r| r.get::<_, String>(0))? {
                paths.insert(PathBuf::from(path?));
            }
            let mut query = conn.prepare("SELECT DISTINCT content_key FROM media WHERE shoot_id = ?1 AND media_type = 'video'")?;
            for key in query.query_map([shoot_id], |r| r.get::<_, String>(0))? {
                paths.insert(state.proxies.path_for(&key?));
            }
            (record_bytes, paths)
        };
        let mut preview_bytes = 0;
        for path in paths {
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => preview_bytes += meta.len(),
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(e) => return Err(e.into()),
            }
        }
        Ok(ShootStorage { record_bytes, preview_bytes })
    }).await?
}
