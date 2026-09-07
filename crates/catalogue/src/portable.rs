use std::{io::Cursor, path::Path};

use rusqlite::{params, params_from_iter, types::Value, Connection};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{normalize_relative_path, CatalogueError, Result};

pub(crate) const PORTABLE_SCHEMA: &str = r#"
PRAGMA application_id = 1397442372;
PRAGMA user_version = 1;
CREATE TABLE package_info (schema_version INTEGER NOT NULL, library_id TEXT NOT NULL, shoot_id TEXT NOT NULL, published_revision INTEGER NOT NULL);
CREATE TABLE shoot (id INTEGER PRIMARY KEY, stable_id TEXT NOT NULL, library_id TEXT NOT NULL, name TEXT NOT NULL, status TEXT NOT NULL, notes TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE people (id INTEGER PRIMARY KEY, stable_id TEXT NOT NULL, name TEXT NOT NULL, team TEXT, notes TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE media (
 id INTEGER PRIMARY KEY, stable_id TEXT NOT NULL, shoot_id INTEGER NOT NULL, library_id TEXT NOT NULL,
 relative_path TEXT NOT NULL, filename TEXT NOT NULL, media_type TEXT NOT NULL, extension TEXT NOT NULL,
 width INTEGER, height INTEGER, duration REAL, fps REAL, bitrate INTEGER, video_codec TEXT, audio_codec TEXT,
 file_size INTEGER NOT NULL, captured_at TEXT, camera_make TEXT, camera_model TEXT, lens TEXT, iso INTEGER,
 focal_length REAL, aperture REAL, shutter TEXT, orientation INTEGER NOT NULL, processing_status TEXT NOT NULL,
 face_count INTEGER NOT NULL, person_count INTEGER NOT NULL, recognition_state TEXT,
 quality_score REAL, sharpness_score REAL, exposure_score REAL, duplicate_group_id INTEGER,
 duplicate_count INTEGER NOT NULL, is_best_shot INTEGER NOT NULL, rating INTEGER NOT NULL, pick_state TEXT NOT NULL,
 UNIQUE(library_id, relative_path)
);
CREATE INDEX idx_media_type ON media(media_type);
CREATE INDEX idx_media_path ON media(library_id, relative_path);
CREATE TABLE clusters (id INTEGER PRIMARY KEY, stable_id TEXT NOT NULL, label TEXT NOT NULL, person_id INTEGER, status TEXT NOT NULL, face_count INTEGER NOT NULL, cover_face_id INTEGER, created_at TEXT NOT NULL);
CREATE TABLE faces (
 id INTEGER PRIMARY KEY, media_id INTEGER NOT NULL, person_id INTEGER, cluster_id INTEGER,
 bbox_x REAL NOT NULL, bbox_y REAL NOT NULL, bbox_w REAL NOT NULL, bbox_h REAL NOT NULL,
 landmarks BLOB, detection_confidence REAL NOT NULL, recognition_confidence REAL,
 assignment TEXT NOT NULL, quality REAL, frame_time REAL, source TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE INDEX idx_faces_media ON faces(media_id);
CREATE TABLE video_detections (id INTEGER PRIMARY KEY, media_id INTEGER NOT NULL, person_id INTEGER, face_id INTEGER, timestamp REAL NOT NULL, end_timestamp REAL, confidence REAL NOT NULL);
CREATE TABLE video_sample_frames (media_id INTEGER NOT NULL, timestamp REAL NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY(media_id, timestamp));
CREATE TABLE albums (id INTEGER PRIMARY KEY, stable_id TEXT NOT NULL, name TEXT NOT NULL, album_type TEXT NOT NULL, person_ids TEXT, cluster_id INTEGER, cover_media_id INTEGER, media_count INTEGER NOT NULL, photo_count INTEGER NOT NULL, video_count INTEGER NOT NULL, sort_order INTEGER NOT NULL, generated_at TEXT NOT NULL);
CREATE TABLE album_media (album_id INTEGER NOT NULL, media_id INTEGER NOT NULL, PRIMARY KEY(album_id, media_id));
CREATE TABLE groups (id INTEGER PRIMARY KEY, stable_id TEXT NOT NULL, name TEXT NOT NULL, folder_name TEXT, notes TEXT, person_id INTEGER, sort_order INTEGER NOT NULL, media_count INTEGER NOT NULL, photo_count INTEGER NOT NULL, video_count INTEGER NOT NULL, cover_media_id INTEGER, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE group_media (group_id INTEGER NOT NULL, media_id INTEGER NOT NULL, added_at TEXT NOT NULL, PRIMARY KEY(group_id, media_id));
"#;

const ALLOWED_TABLES: &[&str] = &[
    "package_info",
    "shoot",
    "people",
    "media",
    "clusters",
    "faces",
    "video_detections",
    "video_sample_frames",
    "albums",
    "album_media",
    "groups",
    "group_media",
];

pub struct PortableCatalogue {
    pub bytes: Zeroizing<Vec<u8>>,
    pub library_id: String,
    pub shoot_id: String,
    pub media_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueSummary {
    pub library_id: String,
    pub shoot_id: String,
    pub shoot_name: String,
    pub published_revision: u64,
    pub media_count: u64,
    pub group_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueGroup {
    pub id: i64,
    pub stable_id: String,
    pub name: String,
    pub folder_name: Option<String>,
    pub notes: Option<String>,
    pub media_count: i64,
    pub photo_count: i64,
    pub video_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueMedia {
    pub id: i64,
    pub stable_id: String,
    pub relative_path: String,
    pub filename: String,
    pub media_type: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<f64>,
    pub rating: i64,
    pub pick_state: String,
    pub is_best_shot: bool,
    pub group_ids: Vec<i64>,
}

pub fn build_portable_catalogue(
    source: &Connection,
    shoot_id: i64,
    published_revision: u64,
) -> Result<PortableCatalogue> {
    let (stable_shoot_id, library_id, source_root): (String, String, String) = source
        .query_row(
            "SELECT stable_id, library_id, source_path FROM shoots WHERE id = ?1 AND tombstone = 0",
            [shoot_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| CatalogueError::Invalid(format!("shoot cannot be published: {error}")))?;

    let mut destination = Connection::open_in_memory()
        .map_err(|error| CatalogueError::Invalid(format!("create portable catalogue: {error}")))?;
    destination.execute_batch(PORTABLE_SCHEMA).map_err(sql_error)?;
    let tx = destination.transaction().map_err(sql_error)?;
    tx.execute(
        "INSERT INTO package_info VALUES (1, ?1, ?2, ?3)",
        params![
            library_id,
            stable_shoot_id,
            i64::try_from(published_revision)
                .map_err(|_| CatalogueError::Invalid("revision number is too large".into()))?
        ],
    )
    .map_err(sql_error)?;

    copy_rows(
        source,
        &tx,
        "SELECT id, stable_id, library_id, name, status, notes, created_at, updated_at FROM shoots WHERE id = ?1",
        "INSERT INTO shoot VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        &[Value::Integer(shoot_id)],
    )?;
    copy_rows(source, &tx,
        "SELECT DISTINCT p.id,p.stable_id,p.name,p.team,p.notes,p.created_at,p.updated_at FROM people p WHERE p.id IN (SELECT person_id FROM faces WHERE shoot_id=?1 AND person_id IS NOT NULL UNION SELECT person_id FROM media_groups WHERE shoot_id=?1 AND person_id IS NOT NULL UNION SELECT person_id FROM clusters WHERE shoot_id=?1 AND person_id IS NOT NULL)",
        "INSERT INTO people VALUES (?1,?2,?3,?4,?5,?6,?7)", &[Value::Integer(shoot_id)])?;

    let media_count = copy_media(source, &tx, shoot_id, &library_id, &source_root)?;
    copy_shoot_tables(source, &tx, shoot_id)?;
    tx.commit().map_err(sql_error)?;
    destination.execute_batch("PRAGMA optimize;").map_err(sql_error)?;
    let data = destination.serialize("main").map_err(sql_error)?;
    let bytes = data.to_vec();
    validate_portable_catalogue(&bytes)?;
    Ok(PortableCatalogue {
        bytes: Zeroizing::new(bytes),
        library_id,
        shoot_id: stable_shoot_id,
        media_count,
    })
}

fn copy_media(
    source: &Connection,
    destination: &Connection,
    shoot_id: i64,
    library_id: &str,
    source_root: &str,
) -> Result<u64> {
    let mut count = 0;
    let mut statement = source.prepare(
        "SELECT id,stable_id,path,normalized_relative_path,filename,media_type,extension,width,height,duration,fps,bitrate,video_codec,audio_codec,file_size,captured_at,camera_make,camera_model,lens,iso,focal_length,aperture,shutter,orientation,processing_status,face_count,person_count,quality_score,sharpness_score,exposure_score,duplicate_group_id,duplicate_count,is_best_shot,rating,pick_state FROM media WHERE shoot_id=?1 AND tombstone=0 ORDER BY id"
    ).map_err(sql_error)?;
    let mut rows = statement.query([shoot_id]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let absolute: String = row.get(2).map_err(sql_error)?;
        let stored_relative: Option<String> = row.get(3).map_err(sql_error)?;
        let relative =
            match stored_relative {
                Some(path) if !path.trim().is_empty() => normalize_relative_path(Path::new(&path))?,
                _ => normalize_relative_path(Path::new(&absolute).strip_prefix(Path::new(source_root)).map_err(
                    |_| CatalogueError::Invalid(format!("media path is outside the shoot root: {absolute}")),
                )?)?,
            };
        let status: String = row.get(24).map_err(sql_error)?;
        let faces: i64 = row.get(25).map_err(sql_error)?;
        destination.execute(
            "INSERT INTO media VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,?36,?37)",
            params![row.get::<_, Value>(0).map_err(sql_error)?, row.get::<_, Value>(1).map_err(sql_error)?, shoot_id, library_id, relative,
                row.get::<_, Value>(4).map_err(sql_error)?, row.get::<_, Value>(5).map_err(sql_error)?, row.get::<_, Value>(6).map_err(sql_error)?,
                row.get::<_, Value>(7).map_err(sql_error)?, row.get::<_, Value>(8).map_err(sql_error)?, row.get::<_, Value>(9).map_err(sql_error)?,
                row.get::<_, Value>(10).map_err(sql_error)?, row.get::<_, Value>(11).map_err(sql_error)?, row.get::<_, Value>(12).map_err(sql_error)?,
                row.get::<_, Value>(13).map_err(sql_error)?, row.get::<_, Value>(14).map_err(sql_error)?, row.get::<_, Value>(15).map_err(sql_error)?,
                row.get::<_, Value>(16).map_err(sql_error)?, row.get::<_, Value>(17).map_err(sql_error)?, row.get::<_, Value>(18).map_err(sql_error)?,
                row.get::<_, Value>(19).map_err(sql_error)?, row.get::<_, Value>(20).map_err(sql_error)?, row.get::<_, Value>(21).map_err(sql_error)?,
                row.get::<_, Value>(22).map_err(sql_error)?, row.get::<_, Value>(23).map_err(sql_error)?, status, faces, row.get::<_, Value>(26).map_err(sql_error)?,
                recognition_state(&status, faces), row.get::<_, Value>(27).map_err(sql_error)?, row.get::<_, Value>(28).map_err(sql_error)?,
                row.get::<_, Value>(29).map_err(sql_error)?, row.get::<_, Value>(30).map_err(sql_error)?, row.get::<_, Value>(31).map_err(sql_error)?,
                row.get::<_, Value>(32).map_err(sql_error)?, row.get::<_, Value>(33).map_err(sql_error)?, row.get::<_, Value>(34).map_err(sql_error)?],
        ).map_err(sql_error)?;
        count += 1;
    }
    Ok(count)
}

fn copy_shoot_tables(source: &Connection, destination: &Connection, shoot_id: i64) -> Result<()> {
    let arg = [Value::Integer(shoot_id)];
    for (select, insert) in [
        ("SELECT id,stable_id,label,person_id,status,face_count,cover_face_id,created_at FROM clusters WHERE shoot_id=?1", "INSERT INTO clusters VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"),
        ("SELECT id,media_id,person_id,cluster_id,bbox_x,bbox_y,bbox_w,bbox_h,landmarks,detection_confidence,recognition_confidence,assignment,quality,frame_time,source,created_at FROM faces WHERE shoot_id=?1", "INSERT INTO faces VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"),
        ("SELECT vd.id,vd.media_id,vd.person_id,vd.face_id,vd.timestamp,vd.end_timestamp,vd.confidence FROM video_detections vd JOIN media m ON m.id=vd.media_id WHERE m.shoot_id=?1", "INSERT INTO video_detections VALUES (?1,?2,?3,?4,?5,?6,?7)"),
        ("SELECT vs.media_id,vs.timestamp,vs.created_at FROM video_sample_frames vs JOIN media m ON m.id=vs.media_id WHERE m.shoot_id=?1", "INSERT INTO video_sample_frames VALUES (?1,?2,?3)"),
        ("SELECT id,stable_id,name,album_type,person_ids,cluster_id,cover_media_id,media_count,photo_count,video_count,sort_order,generated_at FROM albums WHERE shoot_id=?1", "INSERT INTO albums VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"),
        ("SELECT am.album_id,am.media_id FROM album_media am JOIN albums a ON a.id=am.album_id WHERE a.shoot_id=?1", "INSERT INTO album_media VALUES (?1,?2)"),
        ("SELECT id,stable_id,name,folder_name,notes,person_id,sort_order,media_count,photo_count,video_count,cover_media_id,created_at,updated_at FROM media_groups WHERE shoot_id=?1", "INSERT INTO groups VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
        ("SELECT gi.group_id,gi.media_id,gi.added_at FROM media_group_items gi JOIN media_groups g ON g.id=gi.group_id WHERE g.shoot_id=?1", "INSERT INTO group_media VALUES (?1,?2,?3)"),
    ] { copy_rows(source, destination, select, insert, &arg)?; }
    Ok(())
}

fn copy_rows(
    source: &Connection,
    destination: &Connection,
    select_sql: &str,
    insert_sql: &str,
    query_params: &[Value],
) -> Result<()> {
    let mut select = source.prepare(select_sql).map_err(sql_error)?;
    let column_count = select.column_count();
    let mut rows = select.query(params_from_iter(query_params.iter())).map_err(sql_error)?;
    let mut insert = destination.prepare_cached(insert_sql).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let values = (0..column_count)
            .map(|index| row.get::<_, Value>(index).map_err(sql_error))
            .collect::<Result<Vec<_>>>()?;
        insert.execute(params_from_iter(values)).map_err(sql_error)?;
    }
    Ok(())
}

fn recognition_state(processing_status: &str, face_count: i64) -> &'static str {
    if processing_status == "failed" {
        "failed"
    } else if face_count > 0 {
        "recognised"
    } else if processing_status == "done" {
        "no_faces"
    } else {
        "pending"
    }
}

pub fn validate_portable_catalogue(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(b"SQLite format 3\0") {
        return Err(CatalogueError::Invalid("catalogue is not SQLite".into()));
    }
    let mut connection = Connection::open_in_memory().map_err(sql_error)?;
    connection
        .deserialize_read_exact("main", Cursor::new(bytes), bytes.len(), true)
        .map_err(sql_error)?;
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sql_error)?;
    if integrity != "ok" {
        return Err(CatalogueError::Invalid(format!(
            "catalogue integrity check failed: {integrity}"
        )));
    }
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_error)?;
    if version != 1 {
        return Err(CatalogueError::Invalid(format!(
            "unsupported catalogue schema {version}"
        )));
    }
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .map_err(sql_error)?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?;
    for table in tables {
        let table = table.map_err(sql_error)?;
        if !ALLOWED_TABLES.contains(&table.as_str()) {
            return Err(CatalogueError::Invalid(format!("unexpected catalogue table {table}")));
        }
    }
    Ok(())
}

pub fn catalogue_summary(bytes: &[u8]) -> Result<CatalogueSummary> {
    let connection = open_portable(bytes)?;
    let (library_id, shoot_id, revision): (String, String, i64) = connection
        .query_row(
            "SELECT library_id,shoot_id,published_revision FROM package_info",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql_error)?;
    let shoot_name = connection
        .query_row("SELECT name FROM shoot LIMIT 1", [], |row| row.get(0))
        .map_err(sql_error)?;
    let media_count: i64 = connection
        .query_row("SELECT count(*) FROM media", [], |row| row.get(0))
        .map_err(sql_error)?;
    let group_count: i64 = connection
        .query_row("SELECT count(*) FROM groups", [], |row| row.get(0))
        .map_err(sql_error)?;
    Ok(CatalogueSummary {
        library_id,
        shoot_id,
        shoot_name,
        published_revision: revision
            .try_into()
            .map_err(|_| CatalogueError::Invalid("negative revision".into()))?,
        media_count: media_count as u64,
        group_count: group_count as u64,
    })
}

pub fn catalogue_groups(bytes: &[u8]) -> Result<Vec<CatalogueGroup>> {
    let connection = open_portable(bytes)?;
    let mut statement = connection.prepare("SELECT id,stable_id,name,folder_name,notes,media_count,photo_count,video_count FROM groups ORDER BY sort_order,name COLLATE NOCASE").map_err(sql_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(CatalogueGroup {
                id: row.get(0)?,
                stable_id: row.get(1)?,
                name: row.get(2)?,
                folder_name: row.get(3)?,
                notes: row.get(4)?,
                media_count: row.get(5)?,
                photo_count: row.get(6)?,
                video_count: row.get(7)?,
            })
        })
        .map_err(sql_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)
}

pub fn catalogue_media(bytes: &[u8], group_id: Option<i64>) -> Result<Vec<CatalogueMedia>> {
    let connection = open_portable(bytes)?;
    let sql = "SELECT m.id,m.stable_id,m.relative_path,m.filename,m.media_type,m.width,m.height,m.duration,m.rating,m.pick_state,m.is_best_shot,(SELECT group_concat(group_id, ',') FROM group_media WHERE media_id=m.id) FROM media m WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM group_media gm WHERE gm.media_id=m.id AND gm.group_id=?1)) ORDER BY m.captured_at,m.filename";
    let mut statement = connection.prepare(sql).map_err(sql_error)?;
    let rows = statement
        .query_map([group_id], |row| {
            let group_list: Option<String> = row.get(11)?;
            Ok(CatalogueMedia {
                id: row.get(0)?,
                stable_id: row.get(1)?,
                relative_path: row.get(2)?,
                filename: row.get(3)?,
                media_type: row.get(4)?,
                width: row.get(5)?,
                height: row.get(6)?,
                duration: row.get(7)?,
                rating: row.get(8)?,
                pick_state: row.get(9)?,
                is_best_shot: row.get::<_, i64>(10)? != 0,
                group_ids: group_list
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|id| id.parse().ok())
                    .collect(),
            })
        })
        .map_err(sql_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)
}

fn open_portable(bytes: &[u8]) -> Result<Connection> {
    validate_portable_catalogue(bytes)?;
    let mut connection = Connection::open_in_memory().map_err(sql_error)?;
    connection
        .deserialize_read_exact("main", Cursor::new(bytes), bytes.len(), true)
        .map_err(sql_error)?;
    Ok(connection)
}

fn sql_error(error: rusqlite::Error) -> CatalogueError {
    CatalogueError::Invalid(format!("catalogue database error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skwad_database::Database;

    #[test]
    fn exports_only_relative_references_and_never_embeddings_or_crops() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        conn.execute("INSERT INTO shoots(id,name,source_path,status,created_at,updated_at) VALUES(1,'Finals','D:\\NAS\\Finals','done','now','now')", []).unwrap();
        conn.execute(
            "INSERT INTO people(id,name,created_at,updated_at) VALUES(1,'Player Secret','now','now')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO media(id,shoot_id,path,filename,media_type,extension,file_size,content_key,indexed_at,processing_status,face_count) VALUES(1,1,'D:\\NAS\\Finals\\Day1\\frame.jpg','frame.jpg','photo','jpg',123,'key','now','done',1)", []).unwrap();
        conn.execute("INSERT INTO faces(id,media_id,shoot_id,person_id,embedding,embedding_dim,bbox_x,bbox_y,bbox_w,bbox_h,detection_confidence,assignment,crop_path,created_at) VALUES(1,1,1,1,x'DEADBEEF',1,.1,.2,.3,.4,.9,'confirmed','C:\\secret-crop.jpg','now')", []).unwrap();

        let portable = build_portable_catalogue(&conn, 1, 7).unwrap();
        validate_portable_catalogue(&portable.bytes).unwrap();
        let mut opened = Connection::open_in_memory().unwrap();
        opened
            .deserialize_read_exact("main", Cursor::new(&portable.bytes), portable.bytes.len(), true)
            .unwrap();
        let path: String = opened
            .query_row("SELECT relative_path FROM media", [], |row| row.get(0))
            .unwrap();
        assert_eq!(path, "Day1/frame.jpg");
        let face_columns: Vec<String> = opened
            .prepare("PRAGMA table_info(faces)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!face_columns
            .iter()
            .any(|name| name == "embedding" || name == "crop_path"));
        let tables: Vec<String> = opened
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!tables
            .iter()
            .any(|name| matches!(name.as_str(), "jobs" | "settings" | "app_log" | "exports")));
    }

    #[test]
    fn ten_thousand_references_render_within_five_seconds() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(PORTABLE_SCHEMA).unwrap();
        let tx = connection.transaction().unwrap();
        tx.execute("INSERT INTO package_info VALUES(1,'lib','shoot',1)", [])
            .unwrap();
        tx.execute(
            "INSERT INTO shoot VALUES(1,'shoot','lib','Large shoot','done',NULL,'now','now')",
            [],
        )
        .unwrap();
        tx.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO media(id,stable_id,shoot_id,library_id,relative_path,filename,media_type,extension,file_size,orientation,processing_status,face_count,person_count,duplicate_count,is_best_shot,rating,pick_state) SELECT x,printf('media-%d',x),1,'lib',printf('day/file-%d.jpg',x),printf('file-%d.jpg',x),'photo','jpg',100,1,'done',0,0,1,0,0,'none' FROM n;").unwrap();
        tx.commit().unwrap();
        let bytes = connection.serialize("main").unwrap().to_vec();
        let started = std::time::Instant::now();
        let media = catalogue_media(&bytes, None).unwrap();
        assert_eq!(media.len(), 10_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "render query took {:?}",
            started.elapsed()
        );
    }
}
