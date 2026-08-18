use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{Shoot, ShootStatus, ShootSummary};
use crate::{now, Result};

fn map(row: &Row<'_>) -> rusqlite::Result<Shoot> {
    Ok(Shoot {
        id: get(row, "id")?,
        name: get(row, "name")?,
        source_path: get(row, "source_path")?,
        status: get(row, "status")?,
        notes: get(row, "notes")?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

pub fn create(conn: &Connection, name: &str, source_path: &str) -> Result<Shoot> {
    let ts = now();
    conn.execute(
        "INSERT INTO shoots (name, source_path, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![name, source_path, ShootStatus::Created, ts],
    )?;
    let id = conn.last_insert_rowid();
    get_by_id(conn, id)?.ok_or_else(|| crate::DbError::other("shoot vanished after insert"))
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<Shoot>> {
    Ok(conn
        .prepare("SELECT * FROM shoots WHERE id = ?1")?
        .query_row(params![id], map)
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<Shoot>> {
    let mut stmt = conn.prepare("SELECT * FROM shoots ORDER BY created_at DESC, id DESC")?;
    let rows = stmt.query_map([], map)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The Shoots screen listing: one row per shoot with its counts already rolled
/// up, so the UI never has to fan out N+1 queries.
pub fn list_summaries(conn: &Connection) -> Result<Vec<ShootSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.*,
                (SELECT COUNT(*) FROM media m WHERE m.shoot_id = s.id AND m.media_type = 'photo')  AS photo_count,
                (SELECT COUNT(*) FROM media m WHERE m.shoot_id = s.id AND m.media_type = 'video')  AS video_count,
                (SELECT COUNT(*) FROM faces f WHERE f.shoot_id = s.id)                             AS face_count,
                (SELECT COUNT(DISTINCT f.person_id) FROM faces f
                   WHERE f.shoot_id = s.id AND f.person_id IS NOT NULL
                     AND f.assignment IN ('suggested', 'confirmed'))                               AS person_count,
                (SELECT COUNT(*) FROM clusters c WHERE c.shoot_id = s.id AND c.status = 'unnamed') AS unknown_cluster_count,
                (SELECT COUNT(*) FROM jobs j WHERE j.shoot_id = s.id AND j.state IN ('queued','running')) AS pending_jobs,
                (SELECT COUNT(*) FROM jobs j WHERE j.shoot_id = s.id AND j.state = 'failed')       AS failed_jobs
           FROM shoots s
          ORDER BY s.created_at DESC, s.id DESC",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(ShootSummary {
                shoot: map(row)?,
                photo_count: get(row, "photo_count")?,
                video_count: get(row, "video_count")?,
                face_count: get(row, "face_count")?,
                person_count: get(row, "person_count")?,
                unknown_cluster_count: get(row, "unknown_cluster_count")?,
                pending_jobs: get(row, "pending_jobs")?,
                failed_jobs: get(row, "failed_jobs")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn summary(conn: &Connection, id: i64) -> Result<Option<ShootSummary>> {
    Ok(list_summaries(conn)?.into_iter().find(|s| s.shoot.id == id))
}

pub fn set_status(conn: &Connection, id: i64, status: ShootStatus) -> Result<()> {
    conn.execute(
        "UPDATE shoots SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status, now()],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE shoots SET name = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, name, now()],
    )?;
    Ok(())
}

pub fn set_notes(conn: &Connection, id: i64, notes: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE shoots SET notes = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, notes, now()],
    )?;
    Ok(())
}

/// Removes the shoot's **index** only. Faces, albums and jobs cascade away;
/// the user's media on disk is untouched (§21).
pub fn delete_index(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM shoots WHERE id = ?1", params![id])?;
    Ok(())
}

/// Removes every scanned shoot index. Shoot-owned media, faces, clusters,
/// albums, jobs and exports cascade away; global settings and player profiles
/// are deliberately retained. Source media is never touched.
pub fn clear_all_indexes(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM shoots", [], |row| row.get(0))?;
    conn.execute("DELETE FROM shoots", [])?;
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn create_and_summarise() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();

        let shoot = create(&conn, "BGMS Finals Player Shoot", "D:\\BGMS_Final_Shoot").unwrap();
        assert_eq!(shoot.status, "created");

        let summaries = list_summaries(&conn).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].photo_count, 0);
        assert_eq!(summaries[0].video_count, 0);

        set_status(&conn, shoot.id, ShootStatus::Completed).unwrap();
        assert_eq!(get_by_id(&conn, shoot.id).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn clearing_scanned_indexes_preserves_settings() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        create(&conn, "One", "D:\\one").unwrap();
        create(&conn, "Two", "D:\\two").unwrap();
        conn.execute("INSERT INTO settings (key, value) VALUES ('theme', 'dark')", [])
            .unwrap();

        assert_eq!(clear_all_indexes(&conn).unwrap(), 2);
        assert!(list(&conn).unwrap().is_empty());
        let setting: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'theme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(setting, "dark");
    }
}
