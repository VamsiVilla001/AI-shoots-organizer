use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{Person, PersonSummary};
use crate::{now, Result};

fn map(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: get(row, "id")?,
        name: get(row, "name")?,
        team: get(row, "team")?,
        notes: get(row, "notes")?,
        cover_face_id: get(row, "cover_face_id")?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

/// Creates a player, or returns the existing one if the name is already taken.
/// Names are compared case-insensitively so "jonathan" and "Jonathan" are the
/// same player.
pub fn get_or_create(conn: &Connection, name: &str, team: Option<&str>) -> Result<Person> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::DbError::other("player name cannot be empty"));
    }
    if let Some(existing) = find_by_name(conn, name)? {
        return Ok(existing);
    }
    let ts = now();
    conn.execute(
        "INSERT INTO people (name, team, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![name, team, ts],
    )?;
    let id = conn.last_insert_rowid();
    get_by_id(conn, id)?.ok_or_else(|| crate::DbError::other("person vanished after insert"))
}

pub fn find_by_name(conn: &Connection, name: &str) -> Result<Option<Person>> {
    Ok(conn
        .prepare("SELECT * FROM people WHERE name = ?1 COLLATE NOCASE")?
        .query_row(params![name.trim()], map)
        .optional()?)
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<Person>> {
    Ok(conn
        .prepare("SELECT * FROM people WHERE id = ?1")?
        .query_row(params![id], map)
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<Person>> {
    let mut stmt = conn.prepare("SELECT * FROM people ORDER BY name COLLATE NOCASE")?;
    let rows = stmt.query_map([], map)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The Players screen listing (§22): face samples, media reach and how many
/// shoots the player has appeared in.
pub fn list_summaries(conn: &Connection, shoot_id: Option<i64>) -> Result<Vec<PersonSummary>> {
    // A single optional filter is applied inside the sub-selects rather than as
    // a join so players with no faces in this shoot still appear, at zero.
    let mut stmt = conn.prepare(
        "SELECT p.*,
                (SELECT COUNT(*) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment = 'confirmed' AND f.embedding IS NOT NULL
                     AND (?1 IS NULL OR f.shoot_id = ?1))                        AS face_sample_count,
                (SELECT COUNT(DISTINCT f.media_id) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment IN ('suggested','confirmed')
                     AND (?1 IS NULL OR f.shoot_id = ?1))                        AS media_count,
                (SELECT COUNT(DISTINCT f.shoot_id) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment IN ('suggested','confirmed')) AS shoot_count
           FROM people p
          ORDER BY media_count DESC, p.name COLLATE NOCASE",
    )?;

    let rows = stmt
        .query_map(params![shoot_id], |row| {
            Ok(PersonSummary {
                person: map(row)?,
                face_sample_count: get(row, "face_sample_count")?,
                media_count: get(row, "media_count")?,
                shoot_count: get(row, "shoot_count")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn rename(conn: &Connection, id: i64, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::DbError::other("player name cannot be empty"));
    }
    if let Some(other) = find_by_name(conn, name)? {
        if other.id != id {
            return Err(crate::DbError::other(format!("a player named \"{name}\" already exists")));
        }
    }
    conn.execute(
        "UPDATE people SET name = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, name, now()],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, id: i64, team: Option<&str>, notes: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE people SET team = ?2, notes = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, team, notes, now()],
    )?;
    Ok(())
}

pub fn set_cover_face(conn: &Connection, id: i64, face_id: Option<i64>) -> Result<()> {
    conn.execute("UPDATE people SET cover_face_id = ?2 WHERE id = ?1", params![id, face_id])?;
    Ok(())
}

/// Folds `source` into `target`: every face and cluster moves across and the
/// source profile is removed. Used by "Merge two people" in the review screen.
pub fn merge(conn: &Connection, target_id: i64, source_id: i64) -> Result<i64> {
    if target_id == source_id {
        return Err(crate::DbError::other("cannot merge a player into itself"));
    }
    conn.execute(
        "UPDATE faces SET person_id = ?1 WHERE person_id = ?2",
        params![target_id, source_id],
    )?;
    conn.execute(
        "UPDATE clusters SET person_id = ?1 WHERE person_id = ?2",
        params![target_id, source_id],
    )?;
    conn.execute(
        "UPDATE video_detections SET person_id = ?1 WHERE person_id = ?2",
        params![target_id, source_id],
    )?;
    let moved = conn.query_row(
        "SELECT COUNT(*) FROM faces WHERE person_id = ?1",
        params![target_id],
        |r| r.get::<_, i64>(0),
    )?;
    conn.execute("DELETE FROM people WHERE id = ?1", params![source_id])?;
    conn.execute("UPDATE people SET updated_at = ?2 WHERE id = ?1", params![target_id, now()])?;
    Ok(moved)
}

/// Drops the player's biometric data but keeps the profile — the "Delete
/// Recognition Data" action in §22, and part of the privacy controls in §24.
pub fn clear_recognition_data(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE faces SET person_id = NULL, recognition_confidence = NULL, assignment = 'unassigned'
          WHERE person_id = ?1",
        params![id],
    )?;
    conn.execute("UPDATE clusters SET person_id = NULL, status = 'unnamed' WHERE person_id = ?1", params![id])?;
    conn.execute("UPDATE people SET cover_face_id = NULL, updated_at = ?2 WHERE id = ?1", params![id, now()])?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM people WHERE id = ?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn get_or_create_is_case_insensitive() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let a = get_or_create(&conn, "Jonathan", None).unwrap();
        let b = get_or_create(&conn, "jonathan", None).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn rename_rejects_a_taken_name() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let a = get_or_create(&conn, "Jonathan", None).unwrap();
        get_or_create(&conn, "Mavi", None).unwrap();
        assert!(rename(&conn, a.id, "Mavi").is_err());
        assert!(rename(&conn, a.id, "Jonathan Amaral").is_ok());
    }
}
