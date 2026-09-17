use postgres::types::ToSql;
use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{BoundingBox, Face, FaceAssignment, FaceQuery, FaceWithContext, NewFace};
use crate::{blob_to_vec, now, params, vec_to_blob, Result};

fn map(row: &Row) -> Result<Face> {
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

pub fn insert(conn: &mut dyn Db, face: &NewFace) -> Result<i64> {
    // Embeddings cross the wire as the same little-endian f32 bytes SQLite held
    // in a BLOB; only the column type changed (BYTEA), not the encoding.
    let embedding = face.embedding.as_ref().map(|e| vec_to_blob(e));
    let dim = face.embedding.as_ref().map(|e| e.len() as i64);
    let landmarks = face.landmarks.as_ref().map(|l| vec_to_blob(l));

    let row = conn.row_one(
        "INSERT INTO faces (media_id, shoot_id, embedding, embedding_dim,
                            bbox_x, bbox_y, bbox_w, bbox_h, landmarks,
                            detection_confidence, quality, frame_time, crop_path, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         RETURNING id",
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
    get(&row, "id")
}

/// Inserts a reviewer-drawn face. Its source marker keeps it safe when the
/// detector is run over the same photograph again.
pub fn insert_manual(conn: &mut dyn Db, face: &NewFace) -> Result<i64> {
    let embedding = face.embedding.as_ref().map(|e| vec_to_blob(e));
    let dim = face.embedding.as_ref().map(|e| e.len() as i64);

    let row = conn.row_one(
        "INSERT INTO faces (media_id, shoot_id, embedding, embedding_dim,
                            bbox_x, bbox_y, bbox_w, bbox_h, landmarks,
                            detection_confidence, source, quality, frame_time, crop_path, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, 1.0, 'manual', $9, $10, NULL, $11)
         RETURNING id",
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
            face.frame_time,
            now(),
        ],
    )?;
    get(&row, "id")
}

pub fn get_by_id(conn: &mut dyn Db, id: i64) -> Result<Option<Face>> {
    conn.row_opt("SELECT * FROM faces WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

/// Clears every face detected for a media file. Called before re-analysing so
/// a second pass does not double-count.
pub fn delete_for_media(conn: &mut dyn Db, media_id: i64) -> Result<usize> {
    let deleted = conn.exec(
        "DELETE FROM faces WHERE media_id = $1 AND source != 'manual'",
        params![media_id],
    )?;
    Ok(deleted as usize)
}

/// One embedding, with just enough context for the matcher and clusterer.
#[derive(Debug, Clone)]
pub struct FaceVector {
    pub face_id: i64,
    pub media_id: i64,
    pub frame_time: Option<f64>,
    pub person_id: Option<i64>,
    pub embedding: Vec<f32>,
    pub quality: f64,
}

fn map_vector(row: &Row) -> Result<Option<FaceVector>> {
    let blob: Vec<u8> = get(row, "embedding")?;
    let Some(embedding) = blob_to_vec(&blob) else {
        return Ok(None);
    };
    Ok(Some(FaceVector {
        face_id: get(row, "id")?,
        media_id: get(row, "media_id")?,
        frame_time: get(row, "frame_time")?,
        person_id: get(row, "person_id")?,
        embedding,
        quality: get::<Option<f64>>(row, "quality")?.unwrap_or(0.0),
    }))
}

fn collect_vectors(rows: Vec<Row>) -> Result<Vec<FaceVector>> {
    Ok(rows
        .iter()
        .map(map_vector)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// Every embedding a human has confirmed, across all shoots. This *is* the
/// player face library described in §6.
pub fn library_vectors(conn: &mut dyn Db) -> Result<Vec<FaceVector>> {
    collect_vectors(conn.rows(
        "SELECT id, media_id, frame_time, person_id, embedding, quality FROM faces
          WHERE person_id IS NOT NULL AND assignment = 'confirmed' AND embedding IS NOT NULL
            -- A named cluster is a hypothesis, not dozens of independently
            -- reviewed reference photos. Only its reviewed cover may train
            -- recognition; individually tagged faces have no cluster.
            AND (cluster_id IS NULL OR id = (
                SELECT c.cover_face_id FROM clusters c WHERE c.id = faces.cluster_id
            ))",
        params![],
    )?)
}

/// One person's confirmed reference samples — the input to matching a freshly
/// enrolled person against media that predates their enrollment.
pub fn reference_vectors_for_person(conn: &mut dyn Db, person_id: i64) -> Result<Vec<FaceVector>> {
    collect_vectors(conn.rows(
        "SELECT id, media_id, frame_time, person_id, embedding, quality FROM faces
          WHERE person_id = $1 AND assignment = 'confirmed' AND embedding IS NOT NULL",
        params![person_id],
    )?)
}

/// Embeddings in one shoot that still belong to nobody — the input to clustering.
pub fn unassigned_vectors(conn: &mut dyn Db, shoot_id: i64) -> Result<Vec<FaceVector>> {
    collect_vectors(conn.rows(
        "SELECT id, media_id, frame_time, person_id, embedding, quality FROM faces
          WHERE shoot_id = $1 AND person_id IS NULL
            AND assignment NOT IN ('ignored', 'rejected') AND embedding IS NOT NULL
          ORDER BY id",
        params![shoot_id],
    )?)
}

pub fn vectors_for_media(conn: &mut dyn Db, media_id: i64) -> Result<Vec<FaceVector>> {
    collect_vectors(conn.rows(
        "SELECT id, media_id, frame_time, person_id, embedding, quality FROM faces
          WHERE media_id = $1 AND embedding IS NOT NULL",
        params![media_id],
    )?)
}

/// Records a match the recogniser proposed. Never overwrites a human decision.
pub fn set_suggestion(conn: &mut dyn Db, face_id: i64, person_id: i64, confidence: f64) -> Result<()> {
    conn.exec(
        "UPDATE faces SET person_id = $2, recognition_confidence = $3, assignment = 'suggested'
          WHERE id = $1 AND assignment IN ('unassigned', 'suggested')",
        params![face_id, person_id, confidence],
    )?;
    Ok(())
}

/// Suggestions are derived from the current library and thresholds. Clear
/// them before every recognition pass so tightening Settings actually removes
/// stale false positives while confirmed reviewer decisions remain untouched.
pub fn clear_suggestions_for_shoot(conn: &mut dyn Db, shoot_id: i64) -> Result<usize> {
    let cleared = conn.exec(
        "UPDATE faces SET person_id = NULL, recognition_confidence = NULL, assignment = 'unassigned'
          WHERE shoot_id = $1 AND assignment = 'suggested'",
        params![shoot_id],
    )?;
    Ok(cleared as usize)
}

/// A human decision: this face is this player. Confirmed faces become library
/// samples, which is how a correction improves future recognition (§6).
pub fn assign(conn: &mut dyn Db, face_id: i64, person_id: i64, confidence: Option<f64>) -> Result<()> {
    conn.exec(
        "UPDATE faces SET person_id = $2, recognition_confidence = $3, assignment = 'confirmed' WHERE id = $1",
        params![face_id, person_id, confidence],
    )?;
    Ok(())
}

// The four bulk review actions below each replaced a loop over a prepared
// statement with a single `= ANY($1)`. The review screen sends whole selections
// at once — hundreds of ids — and under SQLite each iteration was an in-process
// call, while here it would have been a network round trip apiece.

pub fn assign_many(conn: &mut dyn Db, face_ids: &[i64], person_id: i64) -> Result<usize> {
    if face_ids.is_empty() {
        return Ok(0);
    }
    let n = conn.exec(
        "UPDATE faces SET person_id = $2, assignment = 'confirmed' WHERE id = ANY($1)",
        params![face_ids, person_id],
    )?;
    Ok(n as usize)
}

/// Confirms the suggestion already on the face, without changing who it points at.
pub fn confirm_many(conn: &mut dyn Db, face_ids: &[i64]) -> Result<usize> {
    if face_ids.is_empty() {
        return Ok(0);
    }
    let n = conn.exec(
        "UPDATE faces SET assignment = 'confirmed' WHERE id = ANY($1) AND person_id IS NOT NULL",
        params![face_ids],
    )?;
    Ok(n as usize)
}

/// "Wrong person": detach the suggestion and send the face back to the unknown
/// pool so clustering can have another go at it.
pub fn reject_many(conn: &mut dyn Db, face_ids: &[i64]) -> Result<usize> {
    if face_ids.is_empty() {
        return Ok(0);
    }
    let n = conn.exec(
        "UPDATE faces SET person_id = NULL, recognition_confidence = NULL,
                          cluster_id = NULL, assignment = 'unassigned'
          WHERE id = ANY($1)",
        params![face_ids],
    )?;
    Ok(n as usize)
}

/// "Remove false face detection" — keeps the row so the detector is not re-run
/// on it, but takes it out of every count and album.
pub fn ignore_many(conn: &mut dyn Db, face_ids: &[i64]) -> Result<usize> {
    if face_ids.is_empty() {
        return Ok(0);
    }
    let n = conn.exec(
        "UPDATE faces SET assignment = 'ignored', person_id = NULL, cluster_id = NULL WHERE id = ANY($1)",
        params![face_ids],
    )?;
    Ok(n as usize)
}

pub fn set_assignment(conn: &mut dyn Db, face_id: i64, assignment: FaceAssignment) -> Result<()> {
    conn.exec(
        "UPDATE faces SET assignment = $2 WHERE id = $1",
        params![face_id, assignment.as_str()],
    )?;
    Ok(())
}

pub fn set_cluster(conn: &mut dyn Db, face_id: i64, cluster_id: Option<i64>) -> Result<()> {
    conn.exec(
        "UPDATE faces SET cluster_id = $2 WHERE id = $1",
        params![face_id, cluster_id],
    )?;
    Ok(())
}

pub fn clear_clusters_for_shoot(conn: &mut dyn Db, shoot_id: i64) -> Result<()> {
    conn.exec(
        "UPDATE faces SET cluster_id = NULL WHERE shoot_id = $1",
        params![shoot_id],
    )?;
    Ok(())
}

pub fn count_for_media(conn: &mut dyn Db, media_id: i64) -> Result<i64> {
    super::at(
        &conn.row_one(
            "SELECT COUNT(*) FROM faces WHERE media_id = $1 AND assignment != 'ignored'",
            params![media_id],
        )?,
        0,
    )
}

/// Faces in one image, for drawing bounding boxes over the preview.
pub fn for_media(conn: &mut dyn Db, media_id: i64) -> Result<Vec<Face>> {
    conn.rows(
        "SELECT * FROM faces WHERE media_id = $1 ORDER BY bbox_x",
        params![media_id],
    )?
    .iter()
    .map(map)
    .collect()
}

/// The review workspace query, joined with the media and names it needs.
pub fn query(conn: &mut dyn Db, q: &FaceQuery) -> Result<Vec<FaceWithContext>> {
    let mut sql = String::from(
        "SELECT f.*, m.path AS media_path, m.filename AS media_filename, m.media_type AS media_type,
                m.thumbnail_path AS thumbnail_path, p.name AS person_name, c.label AS cluster_label
           FROM faces f
           JOIN media m   ON m.id = f.media_id
      LEFT JOIN people p  ON p.id = f.person_id
      LEFT JOIN clusters c ON c.id = f.cluster_id",
    );
    let mut wheres: Vec<String> = Vec::new();
    // `+ Sync` is the only change from the rusqlite version: Postgres parameters
    // cross a thread boundary on their way to the connection.
    let mut args: Vec<Box<dyn ToSql + Sync>> = Vec::new();

    if let Some(shoot_id) = q.shoot_id {
        wheres.push(format!("f.shoot_id = ${}", args.len() + 1));
        args.push(Box::new(shoot_id));
    }
    if let Some(person_id) = q.person_id {
        wheres.push(format!("f.person_id = ${}", args.len() + 1));
        args.push(Box::new(person_id));
    }
    if let Some(cluster_id) = q.cluster_id {
        wheres.push(format!("f.cluster_id = ${}", args.len() + 1));
        args.push(Box::new(cluster_id));
    }
    if let Some(assignment) = &q.assignment {
        wheres.push(format!("f.assignment = ${}", args.len() + 1));
        args.push(Box::new(assignment.clone()));
    }
    if let Some(min) = q.min_confidence {
        wheres.push(format!("COALESCE(f.recognition_confidence, 0) >= ${}", args.len() + 1));
        args.push(Box::new(min));
    }
    if let Some(max) = q.max_confidence {
        wheres.push(format!("COALESCE(f.recognition_confidence, 0) <= ${}", args.len() + 1));
        args.push(Box::new(max));
    }

    if !wheres.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wheres.join(" AND "));
    }
    // Least-certain first: that is where a reviewer's attention is worth most.
    sql.push_str(" ORDER BY COALESCE(f.recognition_confidence, 0) ASC, f.id ASC");

    let limit = q.limit.unwrap_or(300).clamp(1, 5_000);
    sql.push_str(&format!(" LIMIT ${}", args.len() + 1));
    args.push(Box::new(limit));
    sql.push_str(&format!(" OFFSET ${}", args.len() + 1));
    args.push(Box::new(q.offset.unwrap_or(0).max(0)));

    let refs: Vec<&(dyn ToSql + Sync)> = args.iter().map(|b| b.as_ref()).collect();
    conn.rows(&sql, refs.as_slice())?
        .iter()
        .map(|row| {
            Ok(FaceWithContext {
                face: map(row)?,
                media_path: get(row, "media_path")?,
                media_filename: get(row, "media_filename")?,
                media_type: get(row, "media_type")?,
                thumbnail_path: get(row, "thumbnail_path")?,
                person_name: get(row, "person_name")?,
                cluster_label: get(row, "cluster_label")?,
            })
        })
        .collect()
}

/// Wipes every embedding in the database while leaving the detections in place
/// — the "Delete embeddings" privacy control from §24.
pub fn clear_all_embeddings(conn: &mut dyn Db) -> Result<usize> {
    let cleared = conn.exec("UPDATE faces SET embedding = NULL, embedding_dim = NULL", params![])?;
    Ok(cleared as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaType, NewMedia};
    use crate::repo::{media, people, shoots};
    use crate::Database;

    fn seed_face(conn: &mut dyn Db, embedding: Vec<f32>) -> (i64, i64) {
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
                bbox: BoundingBox {
                    x: 0.1,
                    y: 0.1,
                    w: 0.2,
                    h: 0.3,
                },
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
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (_, face_id) = seed_face(&mut conn, vec![0.1, 0.2, 0.3]);

        assert!(library_vectors(&mut conn).unwrap().is_empty());

        let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
        assign(&mut conn, face_id, person.id, Some(0.98)).unwrap();

        let lib = library_vectors(&mut conn).unwrap();
        assert_eq!(lib.len(), 1);
        assert_eq!(lib[0].person_id, Some(person.id));
        assert_eq!(
            lib[0].embedding,
            vec![0.1, 0.2, 0.3],
            "the f32 bytes survive the BLOB -> BYTEA move unchanged"
        );
    }

    #[test]
    fn suggestion_does_not_overwrite_a_human_decision() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (_, face_id) = seed_face(&mut conn, vec![0.5, 0.5]);
        let jonathan = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
        let mavi = people::get_or_create(&mut conn, "Mavi", None).unwrap();

        assign(&mut conn, face_id, jonathan.id, None).unwrap();
        set_suggestion(&mut conn, face_id, mavi.id, 0.99).unwrap();

        let face = get_by_id(&mut conn, face_id).unwrap().unwrap();
        assert_eq!(face.person_id, Some(jonathan.id));
        assert_eq!(face.assignment, "confirmed");
    }

    #[test]
    fn rejecting_returns_a_face_to_the_unknown_pool() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, face_id) = seed_face(&mut conn, vec![0.3, 0.7]);
        let person = people::get_or_create(&mut conn, "Jelly", None).unwrap();
        set_suggestion(&mut conn, face_id, person.id, 0.71).unwrap();

        reject_many(&mut conn, &[face_id]).unwrap();

        let face = get_by_id(&mut conn, face_id).unwrap().unwrap();
        assert_eq!(face.person_id, None);
        assert_eq!(face.assignment, "unassigned");
        assert_eq!(unassigned_vectors(&mut conn, shoot_id).unwrap().len(), 1);
    }

    #[test]
    fn reanalysis_preserves_reviewer_drawn_faces() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, detected_id) = seed_face(&mut conn, vec![0.3, 0.7]);
        let detected = get_by_id(&mut conn, detected_id).unwrap().unwrap();
        let manual_id = insert_manual(
            &mut conn,
            &NewFace {
                media_id: detected.media_id,
                shoot_id,
                bbox: BoundingBox {
                    x: 0.5,
                    y: 0.2,
                    w: 0.2,
                    h: 0.3,
                },
                landmarks: None,
                detection_confidence: 1.0,
                embedding: Some(vec![0.8, 0.2]),
                quality: Some(0.9),
                frame_time: None,
                crop_path: None,
            },
        )
        .unwrap();

        delete_for_media(&mut conn, detected.media_id).unwrap();

        assert!(get_by_id(&mut conn, detected_id).unwrap().is_none());
        assert!(get_by_id(&mut conn, manual_id).unwrap().is_some());
    }

    #[test]
    fn reviewer_drawn_video_faces_keep_the_sample_timestamp() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, detected_id) = seed_face(&mut conn, vec![0.2, 0.8]);
        let detected = get_by_id(&mut conn, detected_id).unwrap().unwrap();
        let manual_id = insert_manual(
            &mut conn,
            &NewFace {
                media_id: detected.media_id,
                shoot_id,
                bbox: BoundingBox {
                    x: 0.2,
                    y: 0.1,
                    w: 0.3,
                    h: 0.4,
                },
                landmarks: None,
                detection_confidence: 1.0,
                embedding: Some(vec![0.4, 0.6]),
                quality: Some(0.85),
                frame_time: Some(12.5),
                crop_path: None,
            },
        )
        .unwrap();

        assert_eq!(get_by_id(&mut conn, manual_id).unwrap().unwrap().frame_time, Some(12.5));
    }

    /// The dynamic query builder went from `?N` to `$N`; an off-by-one there
    /// would silently bind the wrong filter to the wrong column.
    #[test]
    fn the_query_builder_binds_every_filter_to_its_own_placeholder() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, face_id) = seed_face(&mut conn, vec![0.1, 0.9]);
        let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
        set_suggestion(&mut conn, face_id, person.id, 0.62).unwrap();

        let matched = query(
            &mut conn,
            &FaceQuery {
                shoot_id: Some(shoot_id),
                person_id: Some(person.id),
                cluster_id: None,
                assignment: Some("suggested".into()),
                min_confidence: Some(0.5),
                max_confidence: Some(0.7),
                limit: Some(10),
                offset: Some(0),
            },
        )
        .unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].face.id, face_id);
        assert_eq!(matched[0].person_name.as_deref(), Some("Jonathan"));

        // The same query with a confidence window that excludes it must miss.
        let excluded = query(
            &mut conn,
            &FaceQuery {
                shoot_id: Some(shoot_id),
                person_id: Some(person.id),
                cluster_id: None,
                assignment: Some("suggested".into()),
                min_confidence: Some(0.8),
                max_confidence: None,
                limit: Some(10),
                offset: Some(0),
            },
        )
        .unwrap();
        assert!(excluded.is_empty());
    }
}
