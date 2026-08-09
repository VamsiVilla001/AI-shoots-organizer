use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{Media, MediaMetadata, MediaQuery, NewMedia, ProcessingStatus};
use crate::{now, Result};

fn map(row: &Row<'_>) -> rusqlite::Result<Media> {
    Ok(Media {
        id: get(row, "id")?,
        shoot_id: get(row, "shoot_id")?,
        path: get(row, "path")?,
        filename: get(row, "filename")?,
        media_type: get(row, "media_type")?,
        extension: get(row, "extension")?,
        width: get(row, "width")?,
        height: get(row, "height")?,
        duration: get(row, "duration")?,
        file_size: get(row, "file_size")?,
        content_key: get(row, "content_key")?,
        captured_at: get(row, "captured_at")?,
        indexed_at: get(row, "indexed_at")?,
        camera_make: get(row, "camera_make")?,
        camera_model: get(row, "camera_model")?,
        lens: get(row, "lens")?,
        iso: get(row, "iso")?,
        focal_length: get(row, "focal_length")?,
        aperture: get(row, "aperture")?,
        shutter: get(row, "shutter")?,
        orientation: get(row, "orientation")?,
        thumbnail_path: get(row, "thumbnail_path")?,
        processing_status: get(row, "processing_status")?,
        face_count: get(row, "face_count")?,
        error: get(row, "error")?,
    })
}

/// Inserts a scanned file, or returns the existing id if this shoot already
/// indexed that path. Re-importing a folder is therefore safe and cheap.
pub fn upsert(conn: &Connection, m: &NewMedia) -> Result<i64> {
    conn.execute(
        "INSERT INTO media (shoot_id, path, filename, media_type, extension, file_size, content_key, captured_at, indexed_at)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (shoot_id, path) DO UPDATE SET
              file_size   = excluded.file_size,
              captured_at = COALESCE(media.captured_at, excluded.captured_at),
              -- A changed content key means the file was replaced on disk, so
              -- everything derived from it has to be recomputed.
              processing_status = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.processing_status ELSE 'pending' END,
              thumbnail_path    = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.thumbnail_path ELSE NULL END,
              content_key = excluded.content_key",
        params![
            m.shoot_id,
            m.path,
            m.filename,
            m.media_type,
            m.extension,
            m.file_size,
            m.content_key,
            m.captured_at,
            now(),
        ],
    )?;

    let id: i64 = conn.query_row(
        "SELECT id FROM media WHERE shoot_id = ?1 AND path = ?2",
        params![m.shoot_id, m.path],
        |r| r.get(0),
    )?;
    Ok(id)
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<Media>> {
    Ok(conn
        .prepare("SELECT * FROM media WHERE id = ?1")?
        .query_row(params![id], map)
        .optional()?)
}

pub fn set_metadata(conn: &Connection, id: i64, meta: &MediaMetadata) -> Result<()> {
    conn.execute(
        "UPDATE media SET width = ?2, height = ?3, duration = ?4,
                          captured_at = COALESCE(?5, captured_at),
                          camera_make = ?6, camera_model = ?7, lens = ?8,
                          iso = ?9, focal_length = ?10, aperture = ?11, shutter = ?12,
                          orientation = ?13
          WHERE id = ?1",
        params![
            id,
            meta.width,
            meta.height,
            meta.duration,
            meta.captured_at,
            meta.camera_make,
            meta.camera_model,
            meta.lens,
            meta.iso,
            meta.focal_length,
            meta.aperture,
            meta.shutter,
            meta.orientation,
        ],
    )?;
    Ok(())
}

pub fn set_thumbnail(conn: &Connection, id: i64, thumbnail_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE media SET thumbnail_path = ?2 WHERE id = ?1",
        params![id, thumbnail_path],
    )?;
    Ok(())
}

pub fn set_status(conn: &Connection, id: i64, status: ProcessingStatus, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE media SET processing_status = ?2, error = ?3 WHERE id = ?1",
        params![id, status, error],
    )?;
    Ok(())
}

pub fn refresh_face_count(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE media SET face_count = (SELECT COUNT(*) FROM faces WHERE media_id = ?1 AND assignment != 'ignored')
          WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Paths already indexed for a shoot — used by the scanner to skip work.
pub fn existing_content_keys(conn: &Connection, shoot_id: i64) -> Result<std::collections::HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT path, content_key FROM media WHERE shoot_id = ?1")?;
    let mut out = std::collections::HashMap::new();
    let rows = stmt.query_map(params![shoot_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (path, key) = row?;
        out.insert(path, key);
    }
    Ok(out)
}

/// Files that still need the analysis pipeline run over them.
pub fn pending(conn: &Connection, shoot_id: i64, limit: i64) -> Result<Vec<Media>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM media
          WHERE shoot_id = ?1 AND processing_status IN ('pending', 'indexed', 'thumbnailed')
          ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![shoot_id, limit], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn count_for_shoot(conn: &Connection, shoot_id: i64) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM media WHERE shoot_id = ?1", params![shoot_id], |r| r.get(0))?)
}

/// The media grid query. Built as dynamic SQL because the filters in §23 and
/// §10 combine freely.
pub fn query(conn: &Connection, q: &MediaQuery) -> Result<Vec<Media>> {
    let mut sql = String::from("SELECT DISTINCT m.* FROM media m");
    let mut wheres: Vec<String> = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if q.album_id.is_some() {
        sql.push_str(" JOIN album_media am ON am.media_id = m.id");
    }
    if q.person_id.is_some() || q.cluster_id.is_some() || q.only_unidentified {
        sql.push_str(" JOIN faces f ON f.media_id = m.id");
    }

    if let Some(shoot_id) = q.shoot_id {
        wheres.push(format!("m.shoot_id = ?{}", args.len() + 1));
        args.push(Box::new(shoot_id));
    }
    if let Some(album_id) = q.album_id {
        wheres.push(format!("am.album_id = ?{}", args.len() + 1));
        args.push(Box::new(album_id));
    }
    if let Some(person_id) = q.person_id {
        wheres.push(format!(
            "f.person_id = ?{} AND f.assignment IN ('suggested','confirmed')",
            args.len() + 1
        ));
        args.push(Box::new(person_id));
    }
    if let Some(cluster_id) = q.cluster_id {
        wheres.push(format!("f.cluster_id = ?{}", args.len() + 1));
        args.push(Box::new(cluster_id));
    }
    if q.only_unidentified {
        wheres.push("f.person_id IS NULL AND f.assignment != 'ignored'".to_string());
    }
    if let Some(media_type) = &q.media_type {
        wheres.push(format!("m.media_type = ?{}", args.len() + 1));
        args.push(Box::new(media_type.clone()));
    }
    if let Some(search) = &q.search {
        if !search.trim().is_empty() {
            wheres.push(format!("m.filename LIKE ?{}", args.len() + 1));
            args.push(Box::new(format!("%{}%", search.trim())));
        }
    }

    if !wheres.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wheres.join(" AND "));
    }
    sql.push_str(" ORDER BY m.captured_at IS NULL, m.captured_at, m.filename");

    let limit = q.limit.unwrap_or(500).clamp(1, 5_000);
    sql.push_str(&format!(" LIMIT ?{}", args.len() + 1));
    args.push(Box::new(limit));
    sql.push_str(&format!(" OFFSET ?{}", args.len() + 1));
    args.push(Box::new(q.offset.unwrap_or(0).max(0)));

    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Reverts derived state so a shoot can be re-analysed from scratch without
/// re-scanning the folder.
pub fn reset_analysis(conn: &Connection, shoot_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE media SET processing_status = 'indexed', face_count = 0, error = NULL WHERE shoot_id = ?1",
        params![shoot_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MediaType;
    use crate::repo::shoots;
    use crate::Database;

    fn seed(conn: &Connection) -> i64 {
        let shoot = shoots::create(conn, "Test", "C:\\shoot").unwrap();
        for (i, name) in ["a.jpg", "b.jpg", "c.mp4"].iter().enumerate() {
            upsert(
                conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: format!("C:\\shoot\\{name}"),
                    filename: name.to_string(),
                    media_type: if name.ends_with("mp4") { MediaType::Video } else { MediaType::Photo },
                    extension: name.split('.').next_back().unwrap().to_string(),
                    file_size: 100 + i as i64,
                    content_key: format!("key{i}"),
                    captured_at: None,
                },
            )
            .unwrap();
        }
        shoot.id
    }

    #[test]
    fn upsert_is_idempotent_per_path() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);
        assert_eq!(count_for_shoot(&conn, shoot_id).unwrap(), 3);
        seed(&conn); // a second shoot, not duplicates in the first
        assert_eq!(count_for_shoot(&conn, shoot_id).unwrap(), 3);
    }

    #[test]
    fn changed_content_key_resets_processing() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);
        let id: i64 = conn
            .query_row("SELECT id FROM media WHERE filename = 'a.jpg'", [], |r| r.get(0))
            .unwrap();
        set_status(&conn, id, ProcessingStatus::Analysed, None).unwrap();

        upsert(
            &conn,
            &NewMedia {
                shoot_id,
                path: "C:\\shoot\\a.jpg".into(),
                filename: "a.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 999,
                content_key: "different".into(),
                captured_at: None,
            },
        )
        .unwrap();

        assert_eq!(get_by_id(&conn, id).unwrap().unwrap().processing_status, "pending");
    }

    #[test]
    fn query_filters_by_type() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);
        let videos = query(
            &conn,
            &MediaQuery { shoot_id: Some(shoot_id), media_type: Some("video".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].filename, "c.mp4");
    }
}
