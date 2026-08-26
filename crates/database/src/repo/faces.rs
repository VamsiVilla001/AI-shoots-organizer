use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{BoundingBox, Face, FaceAssignment, FaceQuery, FaceWithContext, NewFace};
use crate::{blob_to_vec, now, vec_to_blob, Result};

fn map(row: &Row<'_>) -> rusqlite::Result<Face> {
    Ok(Face {
        id: get(row, "id")?,
        media_id: get(row, "media_id")?,
        shoot_id: get(row, "shoot_id")?,
        person_id: get(row, "person_id")?,
        cluster_id: get(row, "cluster_id")?,
        embedding_dim: get(row, "embedding_dim")?,
        bbox: BoundingBox {
            x: get(row, "bbox_x")?,
            y: get(row, "bbox_y")?,
            w: get(row, "bbox_w")?,
            h: get(row, "bbox_h")?,
        },
        detection_confidence: get(row, "detection_confidence")?,
        recognition_confidence: get(row, "recognition_confidence")?,
        assignment: get(row, "assignment")?,
        quality: get(row, "quality")?,
        frame_time: get(row, "frame_time")?,
        crop_path: get(row, "crop_path")?,
        created_at: get(row, "created_at")?,
    })
}

pub fn insert(conn: &Connection, face: &NewFace) -> Result<i64> {
    let embedding = face.embedding.as_ref().map(|e| vec_to_blob(e));
    let dim = face.embedding.as_ref().map(|e| e.len() as i64);
    let landmarks = face.landmarks.as_ref().map(|l| vec_to_blob(l));

    conn.execute(
        "INSERT INTO faces (media_id, shoot_id, embedding, embedding_dim,
                            bbox_x, bbox_y, bbox_w, bbox_h, landmarks,
                            detection_confidence, quality, frame_time, crop_path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            face.media_id,
            face.shoot_id,
            embedding,
            dim,
            face.bbox.x,
            face.bbox.y,
            face.bbox.w,
            face.bbox.h,
            landmarks,
            face.detection_confidence,
            face.quality,
            face.frame_time,
            face.crop_path,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Inserts a reviewer-drawn face. Its source marker keeps it safe when the
/// detector is run over the same photograph again.
pub fn insert_manual(conn: &Connection, face: &NewFace) -> Result<i64> {
    let embedding = face.embedding.as_ref().map(|e| vec_to_blob(e));
    let dim = face.embedding.as_ref().map(|e| e.len() as i64);

    conn.execute(
        "INSERT INTO faces (media_id, shoot_id, embedding, embedding_dim,
                            bbox_x, bbox_y, bbox_w, bbox_h, landmarks,
                            detection_confidence, source, quality, frame_time, crop_path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 1.0, 'manual', ?9, NULL, NULL, ?10)",
        params![
            face.media_id,
            face.shoot_id,
            embedding,
            dim,
            face.bbox.x,
            face.bbox.y,
            face.bbox.w,
            face.bbox.h,
            face.quality,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<Face>> {
    Ok(conn
        .prepare("SELECT * FROM faces WHERE id = ?1")?
        .query_row(params![id], map)
        .optional()?)
}

/// Clears every face detected for a media file. Called before re-analysing so
/// a second pass does not double-count.
pub fn delete_for_media(conn: &Connection, media_id: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM faces WHERE media_id = ?1 AND source != 'manual'",
        params![media_id],
    )?)
}

/// One embedding, with just enough context for the matcher and clusterer.
#[derive(Debug, Clone)]
pub struct FaceVector {
    pub face_id: i64,
    pub media_id: i64,
    pub person_id: Option<i64>,
    pub embedding: Vec<f32>,
    pub quality: f64,
}

fn map_vector(row: &Row<'_>) -> rusqlite::Result<Option<FaceVector>> {
    let blob: Vec<u8> = row.get("embedding")?;
    let Some(embedding) = blob_to_vec(&blob) else {
        return Ok(None);
    };
    Ok(Some(FaceVector {
        face_id: row.get("id")?,
        media_id: row.get("media_id")?,
        person_id: row.get("person_id")?,
        embedding,
        quality: row.get::<_, Option<f64>>("quality")?.unwrap_or(0.0),
    }))
}

/// Every embedding a human has confirmed, across all shoots. This *is* the
/// player face library described in §6.
pub fn library_vectors(conn: &Connection) -> Result<Vec<FaceVector>> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, person_id, embedding, quality FROM faces
          WHERE person_id IS NOT NULL AND assignment = 'confirmed' AND embedding IS NOT NULL",
    )?;
    let rows = stmt.query_map([], map_vector)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

/// Embeddings in one shoot that still belong to nobody — the input to clustering.
pub fn unassigned_vectors(conn: &Connection, shoot_id: i64) -> Result<Vec<FaceVector>> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, person_id, embedding, quality FROM faces
          WHERE shoot_id = ?1 AND person_id IS NULL
            AND assignment NOT IN ('ignored', 'rejected') AND embedding IS NOT NULL
          ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![shoot_id], map_vector)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

pub fn vectors_for_media(conn: &Connection, media_id: i64) -> Result<Vec<FaceVector>> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, person_id, embedding, quality FROM faces
          WHERE media_id = ?1 AND embedding IS NOT NULL",
    )?;
    let rows = stmt
        .query_map(params![media_id], map_vector)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

/// Records a match the recogniser proposed. Never overwrites a human decision.
pub fn set_suggestion(conn: &Connection, face_id: i64, person_id: i64, confidence: f64) -> Result<()> {
    conn.execute(
        "UPDATE faces SET person_id = ?2, recognition_confidence = ?3, assignment = 'suggested'
          WHERE id = ?1 AND assignment IN ('unassigned', 'suggested')",
        params![face_id, person_id, confidence],
    )?;
    Ok(())
}

/// A human decision: this face is this player. Confirmed faces become library
/// samples, which is how a correction improves future recognition (§6).
pub fn assign(conn: &Connection, face_id: i64, person_id: i64, confidence: Option<f64>) -> Result<()> {
    conn.execute(
        "UPDATE faces SET person_id = ?2, recognition_confidence = ?3, assignment = 'confirmed' WHERE id = ?1",
        params![face_id, person_id, confidence],
    )?;
    Ok(())
}

pub fn assign_many(conn: &Connection, face_ids: &[i64], person_id: i64) -> Result<usize> {
    let mut stmt = conn.prepare(
        "UPDATE faces SET person_id = ?2, assignment = 'confirmed' WHERE id = ?1",
    )?;
    let mut n = 0;
    for id in face_ids {
        n += stmt.execute(params![id, person_id])?;
    }
    Ok(n)
}

/// Confirms the suggestion already on the face, without changing who it points at.
pub fn confirm_many(conn: &Connection, face_ids: &[i64]) -> Result<usize> {
    let mut stmt = conn.prepare(
        "UPDATE faces SET assignment = 'confirmed' WHERE id = ?1 AND person_id IS NOT NULL",
    )?;
    let mut n = 0;
    for id in face_ids {
        n += stmt.execute(params![id])?;
    }
    Ok(n)
}

/// "Wrong person": detach the suggestion and send the face back to the unknown
/// pool so clustering can have another go at it.
pub fn reject_many(conn: &Connection, face_ids: &[i64]) -> Result<usize> {
    let mut stmt = conn.prepare(
        "UPDATE faces SET person_id = NULL, recognition_confidence = NULL,
                          cluster_id = NULL, assignment = 'unassigned'
          WHERE id = ?1",
    )?;
    let mut n = 0;
    for id in face_ids {
        n += stmt.execute(params![id])?;
    }
    Ok(n)
}

/// "Remove false face detection" — keeps the row so the detector is not re-run
/// on it, but takes it out of every count and album.
pub fn ignore_many(conn: &Connection, face_ids: &[i64]) -> Result<usize> {
    let mut stmt = conn.prepare(
        "UPDATE faces SET assignment = 'ignored', person_id = NULL, cluster_id = NULL WHERE id = ?1",
    )?;
    let mut n = 0;
    for id in face_ids {
        n += stmt.execute(params![id])?;
    }
    Ok(n)
}

pub fn set_assignment(conn: &Connection, face_id: i64, assignment: FaceAssignment) -> Result<()> {
    conn.execute("UPDATE faces SET assignment = ?2 WHERE id = ?1", params![face_id, assignment])?;
    Ok(())
}

pub fn set_cluster(conn: &Connection, face_id: i64, cluster_id: Option<i64>) -> Result<()> {
    conn.execute("UPDATE faces SET cluster_id = ?2 WHERE id = ?1", params![face_id, cluster_id])?;
    Ok(())
}

pub fn clear_clusters_for_shoot(conn: &Connection, shoot_id: i64) -> Result<()> {
    conn.execute("UPDATE faces SET cluster_id = NULL WHERE shoot_id = ?1", params![shoot_id])?;
    Ok(())
}

pub fn count_for_media(conn: &Connection, media_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM faces WHERE media_id = ?1 AND assignment != 'ignored'",
        params![media_id],
        |r| r.get(0),
    )?)
}

/// Faces in one image, for drawing bounding boxes over the preview.
pub fn for_media(conn: &Connection, media_id: i64) -> Result<Vec<Face>> {
    let mut stmt = conn.prepare("SELECT * FROM faces WHERE media_id = ?1 ORDER BY bbox_x")?;
    let rows = stmt
        .query_map(params![media_id], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The review workspace query, joined with the media and names it needs.
pub fn query(conn: &Connection, q: &FaceQuery) -> Result<Vec<FaceWithContext>> {
    let mut sql = String::from(
        "SELECT f.*, m.path AS media_path, m.filename AS media_filename, m.media_type AS media_type,
                m.thumbnail_path AS thumbnail_path, p.name AS person_name, c.label AS cluster_label
           FROM faces f
           JOIN media m   ON m.id = f.media_id
      LEFT JOIN people p  ON p.id = f.person_id
      LEFT JOIN clusters c ON c.id = f.cluster_id",
    );
    let mut wheres: Vec<String> = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(shoot_id) = q.shoot_id {
        wheres.push(format!("f.shoot_id = ?{}", args.len() + 1));
        args.push(Box::new(shoot_id));
    }
    if let Some(person_id) = q.person_id {
        wheres.push(format!("f.person_id = ?{}", args.len() + 1));
        args.push(Box::new(person_id));
    }
    if let Some(cluster_id) = q.cluster_id {
        wheres.push(format!("f.cluster_id = ?{}", args.len() + 1));
        args.push(Box::new(cluster_id));
    }
    if let Some(assignment) = &q.assignment {
        wheres.push(format!("f.assignment = ?{}", args.len() + 1));
        args.push(Box::new(assignment.clone()));
    }
    if let Some(min) = q.min_confidence {
        wheres.push(format!("COALESCE(f.recognition_confidence, 0) >= ?{}", args.len() + 1));
        args.push(Box::new(min));
    }
    if let Some(max) = q.max_confidence {
        wheres.push(format!("COALESCE(f.recognition_confidence, 0) <= ?{}", args.len() + 1));
        args.push(Box::new(max));
    }

    if !wheres.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wheres.join(" AND "));
    }
    // Least-certain first: that is where a reviewer's attention is worth most.
    sql.push_str(" ORDER BY COALESCE(f.recognition_confidence, 0) ASC, f.id ASC");

    let limit = q.limit.unwrap_or(300).clamp(1, 5_000);
    sql.push_str(&format!(" LIMIT ?{}", args.len() + 1));
    args.push(Box::new(limit));
    sql.push_str(&format!(" OFFSET ?{}", args.len() + 1));
    args.push(Box::new(q.offset.unwrap_or(0).max(0)));

    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), |row| {
            Ok(FaceWithContext {
                face: map(row)?,
                media_path: get(row, "media_path")?,
                media_filename: get(row, "media_filename")?,
                media_type: get(row, "media_type")?,
                thumbnail_path: get(row, "thumbnail_path")?,
                person_name: get(row, "person_name")?,
                cluster_label: get(row, "cluster_label")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Wipes every embedding in the database while leaving the detections in place
/// — the "Delete embeddings" privacy control from §24.
pub fn clear_all_embeddings(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("UPDATE faces SET embedding = NULL, embedding_dim = NULL", [])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaType, NewMedia};
    use crate::repo::{media, people, shoots};
    use crate::Database;

    fn seed_face(conn: &Connection, embedding: Vec<f32>) -> (i64, i64) {
        let shoot = shoots::create(conn, "S", "C:\\s").unwrap();
        let media_id = media::upsert(
            conn,
            &NewMedia {
                shoot_id: shoot.id,
                path: format!("C:\\s\\{}.jpg", embedding.len()),
                filename: "x.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: format!("k{}", embedding[0]),
                captured_at: None,
            },
        )
        .unwrap();
        let face_id = insert(
            conn,
            &NewFace {
                media_id,
                shoot_id: shoot.id,
                bbox: BoundingBox { x: 0.1, y: 0.1, w: 0.2, h: 0.3 },
                landmarks: None,
                detection_confidence: 0.97,
                embedding: Some(embedding),
                quality: Some(0.8),
                frame_time: None,
                crop_path: None,
            },
        )
        .unwrap();
        (shoot.id, face_id)
    }

    #[test]
    fn confirmed_faces_become_library_vectors() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (_, face_id) = seed_face(&conn, vec![0.1, 0.2, 0.3]);

        assert!(library_vectors(&conn).unwrap().is_empty());

        let person = people::get_or_create(&conn, "Jonathan", None).unwrap();
        assign(&conn, face_id, person.id, Some(0.98)).unwrap();

        let lib = library_vectors(&conn).unwrap();
        assert_eq!(lib.len(), 1);
        assert_eq!(lib[0].person_id, Some(person.id));
        assert_eq!(lib[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn suggestion_does_not_overwrite_a_human_decision() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (_, face_id) = seed_face(&conn, vec![0.5, 0.5]);
        let jonathan = people::get_or_create(&conn, "Jonathan", None).unwrap();
        let mavi = people::get_or_create(&conn, "Mavi", None).unwrap();

        assign(&conn, face_id, jonathan.id, None).unwrap();
        set_suggestion(&conn, face_id, mavi.id, 0.99).unwrap();

        let face = get_by_id(&conn, face_id).unwrap().unwrap();
        assert_eq!(face.person_id, Some(jonathan.id));
        assert_eq!(face.assignment, "confirmed");
    }

    #[test]
    fn rejecting_returns_a_face_to_the_unknown_pool() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (shoot_id, face_id) = seed_face(&conn, vec![0.3, 0.7]);
        let person = people::get_or_create(&conn, "Jelly", None).unwrap();
        set_suggestion(&conn, face_id, person.id, 0.71).unwrap();

        reject_many(&conn, &[face_id]).unwrap();

        let face = get_by_id(&conn, face_id).unwrap().unwrap();
        assert_eq!(face.person_id, None);
        assert_eq!(face.assignment, "unassigned");
        assert_eq!(unassigned_vectors(&conn, shoot_id).unwrap().len(), 1);
    }

    #[test]
    fn reanalysis_preserves_reviewer_drawn_faces() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (shoot_id, detected_id) = seed_face(&conn, vec![0.3, 0.7]);
        let detected = get_by_id(&conn, detected_id).unwrap().unwrap();
        let manual_id = insert_manual(
            &conn,
            &NewFace {
                media_id: detected.media_id,
                shoot_id,
                bbox: BoundingBox { x: 0.5, y: 0.2, w: 0.2, h: 0.3 },
                landmarks: None,
                detection_confidence: 1.0,
                embedding: Some(vec![0.8, 0.2]),
                quality: Some(0.9),
                frame_time: None,
                crop_path: None,
            },
        )
        .unwrap();

        delete_for_media(&conn, detected.media_id).unwrap();

        assert!(get_by_id(&conn, detected_id).unwrap().is_none());
        assert!(get_by_id(&conn, manual_id).unwrap().is_some());
    }
}
