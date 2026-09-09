//! Logical record bytes, not an allocation of shared SQLite pages or WAL files.
use crate::Result;
use rusqlite::Connection;

pub fn shoot_record_bytes(conn: &Connection, shoot_id: i64) -> Result<u64> {
    let mut total = 0u64;
    // Identifiers and predicates are application constants, never user input.
    for (table, predicate) in [
        ("shoots", "id = ?1"),
        ("media", "shoot_id = ?1"),
        ("faces", "shoot_id = ?1"),
        ("clusters", "shoot_id = ?1"),
        ("albums", "shoot_id = ?1"),
        ("media_groups", "shoot_id = ?1"),
        ("jobs", "shoot_id = ?1"),
        (
            "video_detections",
            "media_id IN (SELECT id FROM media WHERE shoot_id = ?1)",
        ),
        (
            "video_sample_frames",
            "media_id IN (SELECT id FROM media WHERE shoot_id = ?1)",
        ),
        ("album_media", "album_id IN (SELECT id FROM albums WHERE shoot_id = ?1)"),
        (
            "media_group_items",
            "group_id IN (SELECT id FROM media_groups WHERE shoot_id = ?1)",
        ),
    ] {
        let mut schema = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = schema
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let expression = columns.iter().map(|name| {
            format!("CASE typeof(\"{name}\") WHEN 'null' THEN 0 WHEN 'integer' THEN 8 WHEN 'real' THEN 8 ELSE length(CAST(\"{name}\" AS BLOB)) END")
        }).collect::<Vec<_>>().join(" + ");
        total += conn.query_row(
            &format!("SELECT COALESCE(SUM({expression}), 0) FROM {table} WHERE {predicate}"),
            [shoot_id],
            |r| r.get::<_, i64>(0),
        )? as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{repo::shoots, Database};

    #[test]
    fn counts_only_requested_shoot_and_preserves_records() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        assert_eq!(shoot_record_bytes(&conn, 999).unwrap(), 0);
        let first = shoots::create(&conn, "First", "C:/first").unwrap();
        let before = shoot_record_bytes(&conn, first.id).unwrap();
        assert!(before > 0);
        shoots::create(&conn, "Other", "C:/other").unwrap();
        assert_eq!(shoot_record_bytes(&conn, first.id).unwrap(), before);
        conn.execute("UPDATE shoots SET name = name || 'abcd' WHERE id = ?1", [first.id])
            .unwrap();
        assert_eq!(shoot_record_bytes(&conn, first.id).unwrap(), before + 4);
        assert!(shoots::get_by_id(&conn, first.id).unwrap().is_some());
        conn.execute("INSERT INTO media (shoot_id, path, filename, media_type, extension, content_key, indexed_at) VALUES (?1, 'a.jpg', 'a.jpg', 'photo', 'jpg', 'key', 'now')", [first.id]).unwrap();
        let media_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO faces (media_id, shoot_id, embedding, bbox_x, bbox_y, bbox_w, bbox_h, created_at, detection_confidence) VALUES (?1, ?2, zeroblob(2048), 0, 0, 1, 1, 'now', 1)", rusqlite::params![media_id, first.id]).unwrap();
        let with_vector = shoot_record_bytes(&conn, first.id).unwrap();
        conn.execute(
            "UPDATE faces SET embedding = zeroblob(4096) WHERE media_id = ?1",
            [media_id],
        )
        .unwrap();
        assert_eq!(shoot_record_bytes(&conn, first.id).unwrap(), with_vector + 2048);
        let before_frame = shoot_record_bytes(&conn, first.id).unwrap();
        conn.execute(
            "INSERT INTO video_sample_frames (media_id, timestamp, created_at) VALUES (?1, 5, 'now')",
            [media_id],
        )
        .unwrap();
        assert_eq!(shoot_record_bytes(&conn, first.id).unwrap(), before_frame + 19);
    }
}
