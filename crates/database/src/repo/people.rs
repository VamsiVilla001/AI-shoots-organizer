use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{Person, PersonSummary};
use crate::{now, params, Result};

fn map(row: &Row) -> Result<Person> {
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
/// same player — `people.name` carries the `nocase` collation, which is what
/// the SQLite build spelled `COLLATE NOCASE` at each call site.
pub fn get_or_create(conn: &mut dyn Db, name: &str, team: Option<&str>) -> Result<Person> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::DbError::other("player name cannot be empty"));
    }
    let ts = now();
    // One statement instead of select-then-insert: two workers naming the same
    // player concurrently used to race between the two, and the loser hit the
    // unique index. `DO UPDATE` on a no-op column makes `RETURNING` fire for
    // the existing row as well as a freshly inserted one.
    let row = conn.row_one(
        "INSERT INTO people (name, team, created_at, updated_at) VALUES ($1, $2, $3, $3)
         ON CONFLICT (name) DO UPDATE SET updated_at = people.updated_at
         RETURNING *",
        params![name, team, ts],
    )?;
    map(&row)
}

pub fn find_by_name(conn: &mut dyn Db, name: &str) -> Result<Option<Person>> {
    conn.row_opt("SELECT * FROM people WHERE name = $1", params![name.trim()])?
        .as_ref()
        .map(map)
        .transpose()
}

pub fn get_by_id(conn: &mut dyn Db, id: i64) -> Result<Option<Person>> {
    conn.row_opt("SELECT * FROM people WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

pub fn list(conn: &mut dyn Db) -> Result<Vec<Person>> {
    conn.rows("SELECT * FROM people ORDER BY name", params![])?
        .iter()
        .map(map)
        .collect()
}

fn map_summary(row: &Row) -> Result<PersonSummary> {
    Ok(PersonSummary {
        person: map(row)?,
        face_sample_count: get(row, "face_sample_count")?,
        media_count: get(row, "media_count")?,
        shoot_count: get(row, "shoot_count")?,
    })
}

/// The Players screen listing (§22): face samples, media reach and how many
/// shoots the player has appeared in.
pub fn list_summaries(conn: &mut dyn Db, shoot_id: Option<i64>) -> Result<Vec<PersonSummary>> {
    // A single optional filter is applied inside the sub-selects rather than as
    // a join so players with no faces in this shoot still appear, at zero.
    // `$1::bigint` rather than `$1`: Postgres cannot infer a parameter's type
    // from `$1 IS NULL` and rejects the statement without the cast.
    conn.rows(
        "SELECT p.*,
                (SELECT COUNT(*) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment = 'confirmed' AND f.embedding IS NOT NULL
                     AND ($1::bigint IS NULL OR f.shoot_id = $1::bigint))        AS face_sample_count,
                (SELECT COUNT(DISTINCT f.media_id) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment IN ('suggested','confirmed')
                     AND ($1::bigint IS NULL OR f.shoot_id = $1::bigint))        AS media_count,
                (SELECT COUNT(DISTINCT f.shoot_id) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment IN ('suggested','confirmed')) AS shoot_count
           FROM people p
          ORDER BY media_count DESC, p.name",
        params![shoot_id],
    )?
    .iter()
    .map(map_summary)
    .collect()
}

/// The Pre-Process tab's people list: only those with at least one confirmed
/// reference sample in the hidden Reference Library shoot, i.e. people
/// enrolled by name + photo/video rather than named from a cluster or tagged
/// while reviewing a shoot. Counts stay global (not scoped to that shoot) so
/// "files" reflects every match found across the whole library, exactly like
/// `list_summaries`.
pub fn list_enrolled_summaries(conn: &mut dyn Db, reference_shoot_id: i64) -> Result<Vec<PersonSummary>> {
    conn.rows(
        "SELECT p.*,
                (SELECT COUNT(*) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment = 'confirmed' AND f.embedding IS NOT NULL) AS face_sample_count,
                (SELECT COUNT(DISTINCT f.media_id) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment IN ('suggested','confirmed')) AS media_count,
                (SELECT COUNT(DISTINCT f.shoot_id) FROM faces f
                   WHERE f.person_id = p.id AND f.assignment IN ('suggested','confirmed')) AS shoot_count
           FROM people p
          WHERE EXISTS (
                SELECT 1 FROM faces f
                 WHERE f.person_id = p.id AND f.shoot_id = $1 AND f.assignment = 'confirmed'
          )
          ORDER BY p.name",
        params![reference_shoot_id],
    )?
    .iter()
    .map(map_summary)
    .collect()
}

pub fn rename(conn: &mut dyn Db, id: i64, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::DbError::other("player name cannot be empty"));
    }
    if let Some(other) = find_by_name(conn, name)? {
        if other.id != id {
            return Err(crate::DbError::other(format!(
                "a player named \"{name}\" already exists"
            )));
        }
    }
    conn.exec(
        "UPDATE people SET name = $2, updated_at = $3 WHERE id = $1",
        params![id, name, now()],
    )?;
    Ok(())
}

pub fn update(conn: &mut dyn Db, id: i64, team: Option<&str>, notes: Option<&str>) -> Result<()> {
    conn.exec(
        "UPDATE people SET team = $2, notes = $3, updated_at = $4 WHERE id = $1",
        params![id, team, notes, now()],
    )?;
    Ok(())
}

pub fn set_cover_face(conn: &mut dyn Db, id: i64, face_id: Option<i64>) -> Result<()> {
    conn.exec(
        "UPDATE people SET cover_face_id = $2 WHERE id = $1",
        params![id, face_id],
    )?;
    Ok(())
}

/// Folds `source` into `target`: every face and cluster moves across and the
/// source profile is removed. Used by "Merge two people" in the review screen.
pub fn merge(conn: &mut dyn Db, target_id: i64, source_id: i64) -> Result<i64> {
    if target_id == source_id {
        return Err(crate::DbError::other("cannot merge a player into itself"));
    }
    conn.exec(
        "UPDATE faces SET person_id = $1 WHERE person_id = $2",
        params![target_id, source_id],
    )?;
    conn.exec(
        "UPDATE clusters SET person_id = $1 WHERE person_id = $2",
        params![target_id, source_id],
    )?;
    conn.exec(
        "UPDATE video_detections SET person_id = $1 WHERE person_id = $2",
        params![target_id, source_id],
    )?;
    let moved: i64 = super::at(
        &conn.row_one("SELECT COUNT(*) FROM faces WHERE person_id = $1", params![target_id])?,
        0,
    )?;
    conn.exec("DELETE FROM people WHERE id = $1", params![source_id])?;
    conn.exec(
        "UPDATE people SET updated_at = $2 WHERE id = $1",
        params![target_id, now()],
    )?;
    Ok(moved)
}

/// Drops the player's biometric data but keeps the profile — the "Delete
/// Recognition Data" action in §22, and part of the privacy controls in §24.
pub fn clear_recognition_data(conn: &mut dyn Db, id: i64) -> Result<()> {
    conn.exec(
        "UPDATE faces SET person_id = NULL, recognition_confidence = NULL, assignment = 'unassigned'
          WHERE person_id = $1",
        params![id],
    )?;
    conn.exec(
        "UPDATE clusters SET person_id = NULL, status = 'unnamed' WHERE person_id = $1",
        params![id],
    )?;
    conn.exec(
        "UPDATE people SET cover_face_id = NULL, updated_at = $2 WHERE id = $1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn delete(conn: &mut dyn Db, id: i64) -> Result<()> {
    conn.exec("DELETE FROM people WHERE id = $1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn get_or_create_is_case_insensitive() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let a = get_or_create(&mut conn, "Jonathan", None).unwrap();
        let b = get_or_create(&mut conn, "jonathan", None).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(list(&mut conn).unwrap().len(), 1);
        assert_eq!(
            a.name, "Jonathan",
            "the first spelling wins; the second must not rename them"
        );
    }

    #[test]
    fn list_enrolled_summaries_excludes_people_never_confirmed_in_the_reference_shoot() {
        use crate::models::{BoundingBox, MediaType, NewFace, NewMedia};
        use crate::repo::{faces, media, shoots};

        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();

        let reference_shoot = shoots::get_or_create_reference_library(&mut conn).unwrap();
        let tagged_shoot = shoots::create(&mut conn, "Tagged Shoot", "C:\\shoot").unwrap();

        let enrolled = get_or_create(&mut conn, "Enrolled Person", None).unwrap();
        let media_id = media::upsert(
            &mut conn,
            &NewMedia {
                shoot_id: reference_shoot.id,
                path: "C:\\ref\\a.jpg".into(),
                filename: "a.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "ref-a".into(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )
        .unwrap();
        let face_id = faces::insert_manual(
            &mut conn,
            &NewFace {
                media_id,
                shoot_id: reference_shoot.id,
                bbox: BoundingBox {
                    x: 0.1,
                    y: 0.1,
                    w: 0.2,
                    h: 0.2,
                },
                landmarks: None,
                detection_confidence: 1.0,
                embedding: Some(vec![1.0, 0.0]),
                quality: Some(0.9),
                frame_time: None,
                crop_path: None,
                model_key: None,
            },
        )
        .unwrap();
        faces::assign(&mut conn, face_id, enrolled.id, Some(1.0)).unwrap();

        // Someone tagged the normal way — confirmed in a real shoot, never enrolled.
        let tagged = get_or_create(&mut conn, "Tagged Person", None).unwrap();
        let tagged_media_id = media::upsert(
            &mut conn,
            &NewMedia {
                shoot_id: tagged_shoot.id,
                path: "C:\\shoot\\b.jpg".into(),
                filename: "b.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "tagged-b".into(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )
        .unwrap();
        let tagged_face_id = faces::insert(
            &mut conn,
            &NewFace {
                media_id: tagged_media_id,
                shoot_id: tagged_shoot.id,
                bbox: BoundingBox {
                    x: 0.1,
                    y: 0.1,
                    w: 0.2,
                    h: 0.2,
                },
                landmarks: None,
                detection_confidence: 0.95,
                embedding: Some(vec![0.0, 1.0]),
                quality: Some(0.8),
                frame_time: None,
                crop_path: None,
                model_key: None,
            },
        )
        .unwrap();
        faces::assign(&mut conn, tagged_face_id, tagged.id, Some(1.0)).unwrap();

        let enrolled_list = list_enrolled_summaries(&mut conn, reference_shoot.id).unwrap();
        assert_eq!(enrolled_list.len(), 1);
        assert_eq!(enrolled_list[0].person.id, enrolled.id);
    }

    #[test]
    fn rename_rejects_a_taken_name() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let a = get_or_create(&mut conn, "Jonathan", None).unwrap();
        get_or_create(&mut conn, "Mavi", None).unwrap();
        assert!(rename(&mut conn, a.id, "Mavi").is_err());
        assert!(rename(&mut conn, a.id, "mavi").is_err(), "case-insensitively taken");
        assert!(rename(&mut conn, a.id, "Jonathan Amaral").is_ok());
    }
}
