use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{Cluster, ClusterStatus, ClusterSummary};
use crate::{now, params, Result};

fn map(row: &Row) -> Result<Cluster> {
    Ok(Cluster {
        id: get(row, "id")?,
        shoot_id: get(row, "shoot_id")?,
        label: get(row, "label")?,
        person_id: get(row, "person_id")?,
        status: get(row, "status")?,
        face_count: get(row, "face_count")?,
        cover_face_id: get(row, "cover_face_id")?,
        created_at: get(row, "created_at")?,
    })
}

pub fn create(conn: &mut dyn Db, shoot_id: i64, label: &str) -> Result<i64> {
    let row = conn.row_one(
        "INSERT INTO clusters (shoot_id, label, created_at) VALUES ($1, $2, $3) RETURNING id",
        params![shoot_id, label, now()],
    )?;
    get(&row, "id")
}

pub fn get_by_id(conn: &mut dyn Db, id: i64) -> Result<Option<Cluster>> {
    conn.row_opt("SELECT * FROM clusters WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

/// Removes every unnamed cluster in a shoot, ahead of a fresh clustering pass.
/// Clusters a human already named are left alone — re-running the algorithm
/// must never undo an identification.
pub fn clear_unnamed(conn: &mut dyn Db, shoot_id: i64) -> Result<usize> {
    let removed = conn.exec(
        "DELETE FROM clusters WHERE shoot_id = $1 AND status = 'unnamed'",
        params![shoot_id],
    )?;
    Ok(removed as usize)
}

pub fn refresh_counts(conn: &mut dyn Db, shoot_id: i64) -> Result<()> {
    conn.exec(
        "UPDATE clusters SET
             face_count = (SELECT COUNT(*) FROM faces f WHERE f.cluster_id = clusters.id),
             cover_face_id = (SELECT f.id FROM faces f WHERE f.cluster_id = clusters.id
                               ORDER BY COALESCE(f.quality, 0) DESC, f.detection_confidence DESC LIMIT 1)
          WHERE shoot_id = $1",
        params![shoot_id],
    )?;
    // A cluster with nothing left in it is noise, not a person.
    conn.exec(
        "DELETE FROM clusters WHERE shoot_id = $1 AND face_count = 0 AND status = 'unnamed'",
        params![shoot_id],
    )?;
    Ok(())
}

/// The "Needs Review" section of the AI Albums screen (§23).
pub fn list_summaries(conn: &mut dyn Db, shoot_id: i64, include_named: bool) -> Result<Vec<ClusterSummary>> {
    let mut sql = String::from(
        "SELECT c.*,
                (SELECT COUNT(DISTINCT f.media_id) FROM faces f WHERE f.cluster_id = c.id) AS media_count,
                p.name AS person_name,
                cf.media_id AS cover_media_id,
                cm.thumbnail_path AS cover_thumbnail_path
           FROM clusters c
      LEFT JOIN people p  ON p.id = c.person_id
      LEFT JOIN faces cf  ON cf.id = c.cover_face_id
      LEFT JOIN media cm  ON cm.id = cf.media_id
          WHERE c.shoot_id = $1",
    );
    if !include_named {
        sql.push_str(" AND c.status = 'unnamed'");
    }
    sql.push_str(" ORDER BY c.face_count DESC, c.id");

    conn.rows(&sql, params![shoot_id])?
        .iter()
        .map(|row| {
            Ok(ClusterSummary {
                cluster: map(row)?,
                media_count: get(row, "media_count")?,
                person_name: get(row, "person_name")?,
                cover_media_id: get(row, "cover_media_id")?,
                cover_thumbnail_path: get(row, "cover_thumbnail_path")?,
            })
        })
        .collect()
}

/// Naming a cluster confirms its members. Reference-library queries trust only
/// the cluster cover; the rest remain visible decisions without multiplying a
/// possibly imperfect cluster into many training samples.
pub fn name_cluster(conn: &mut dyn Db, cluster_id: i64, person_id: i64) -> Result<usize> {
    conn.exec(
        "UPDATE clusters SET person_id = $2, status = 'named' WHERE id = $1",
        params![cluster_id, person_id],
    )?;
    let n = conn.exec(
        "UPDATE faces SET person_id = $2, assignment = 'confirmed' WHERE cluster_id = $1 AND assignment != 'ignored'",
        params![cluster_id, person_id],
    )?;
    Ok(n as usize)
}

/// Splits the given faces out into a new cluster — the "Split incorrect
/// cluster" action from §10.
pub fn split(conn: &mut dyn Db, cluster_id: i64, face_ids: &[i64], label: &str) -> Result<i64> {
    let cluster =
        get_by_id(conn, cluster_id)?.ok_or_else(|| crate::DbError::other(format!("cluster {cluster_id} not found")))?;
    let new_id = create(conn, cluster.shoot_id, label)?;

    if !face_ids.is_empty() {
        // One statement rather than a loop: `cluster_id = $3` still scopes the
        // move to this cluster's own faces, so a stale id from the UI cannot
        // drag a face out of somebody else's cluster.
        conn.exec(
            "UPDATE faces SET cluster_id = $2, person_id = NULL, recognition_confidence = NULL,
                              assignment = 'unassigned'
              WHERE id = ANY($1) AND cluster_id = $3",
            params![face_ids, new_id, cluster_id],
        )?;
    }
    refresh_counts(conn, cluster.shoot_id)?;
    Ok(new_id)
}

/// Folds `source` into `target`. Both must belong to the same shoot.
pub fn merge(conn: &mut dyn Db, target_id: i64, source_id: i64) -> Result<()> {
    if target_id == source_id {
        return Err(crate::DbError::other("cannot merge a cluster into itself"));
    }
    let target =
        get_by_id(conn, target_id)?.ok_or_else(|| crate::DbError::other(format!("cluster {target_id} not found")))?;
    let source =
        get_by_id(conn, source_id)?.ok_or_else(|| crate::DbError::other(format!("cluster {source_id} not found")))?;
    if target.shoot_id != source.shoot_id {
        return Err(crate::DbError::other("clusters belong to different shoots"));
    }

    conn.exec(
        "UPDATE faces SET cluster_id = $1 WHERE cluster_id = $2",
        params![target_id, source_id],
    )?;
    if let Some(person_id) = target.person_id {
        conn.exec(
            "UPDATE faces SET person_id = $2, assignment = 'confirmed'
              WHERE cluster_id = $1 AND assignment != 'ignored'",
            params![target_id, person_id],
        )?;
    }
    conn.exec("DELETE FROM clusters WHERE id = $1", params![source_id])?;
    refresh_counts(conn, target.shoot_id)?;
    Ok(())
}

pub fn set_status(conn: &mut dyn Db, id: i64, status: ClusterStatus) -> Result<()> {
    conn.exec(
        "UPDATE clusters SET status = $2 WHERE id = $1",
        params![id, status.as_str()],
    )?;
    Ok(())
}

pub fn rename_label(conn: &mut dyn Db, id: i64, label: &str) -> Result<()> {
    conn.exec("UPDATE clusters SET label = $2 WHERE id = $1", params![id, label])?;
    Ok(())
}

pub fn delete(conn: &mut dyn Db, id: i64) -> Result<()> {
    conn.exec("DELETE FROM clusters WHERE id = $1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BoundingBox, MediaType, NewFace, NewMedia};
    use crate::repo::{faces, media, people, shoots};
    use crate::Database;

    fn seed(conn: &mut dyn Db) -> (i64, Vec<i64>) {
        let shoot = shoots::create(conn, "S", "C:\\s").unwrap();
        let mut face_ids = Vec::new();
        for i in 0..4 {
            let media_id = media::upsert(
                conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: format!("C:\\s\\{i}.jpg"),
                    filename: format!("{i}.jpg"),
                    media_type: MediaType::Photo,
                    extension: "jpg".into(),
                    file_size: 1,
                    content_key: format!("k{i}"),
                    captured_at: None,
                    normalized_relative_path: None,
                },
            )
            .unwrap();
            face_ids.push(
                faces::insert(
                    conn,
                    &NewFace {
                        media_id,
                        shoot_id: shoot.id,
                        bbox: BoundingBox {
                            x: 0.0,
                            y: 0.0,
                            w: 0.1,
                            h: 0.1,
                        },
                        landmarks: None,
                        detection_confidence: 0.9,
                        embedding: Some(vec![i as f32, 1.0]),
                        quality: Some(i as f64),
                        frame_time: None,
                        crop_path: None,
                        model_key: None,
                    },
                )
                .unwrap(),
            );
        }
        (shoot.id, face_ids)
    }

    #[test]
    fn naming_a_cluster_confirms_all_its_faces() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, face_ids) = seed(&mut conn);
        let cluster_id = create(&mut conn, shoot_id, "Unknown Person 1").unwrap();
        for id in &face_ids {
            faces::set_cluster(&mut conn, *id, Some(cluster_id)).unwrap();
        }
        refresh_counts(&mut conn, shoot_id).unwrap();

        let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
        assert_eq!(name_cluster(&mut conn, cluster_id, person.id).unwrap(), 4);

        // All four remain named in this explicitly reviewed cluster, but only
        // its best cover is trusted as a reusable reference. One bad cluster
        // must not multiply into a contaminated player profile.
        assert_eq!(faces::library_vectors(&mut conn).unwrap().len(), 1);
        for face_id in &face_ids {
            let face = faces::get_by_id(&mut conn, *face_id).unwrap().unwrap();
            assert_eq!(face.person_id, Some(person.id));
            assert_eq!(face.assignment, "confirmed");
        }
        assert_eq!(get_by_id(&mut conn, cluster_id).unwrap().unwrap().status, "named");
    }

    #[test]
    fn split_moves_faces_into_a_new_cluster() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, face_ids) = seed(&mut conn);
        let cluster_id = create(&mut conn, shoot_id, "Unknown Person 1").unwrap();
        for id in &face_ids {
            faces::set_cluster(&mut conn, *id, Some(cluster_id)).unwrap();
        }
        refresh_counts(&mut conn, shoot_id).unwrap();

        let new_id = split(&mut conn, cluster_id, &face_ids[2..], "Unknown Person 2").unwrap();
        assert_eq!(get_by_id(&mut conn, cluster_id).unwrap().unwrap().face_count, 2);
        assert_eq!(get_by_id(&mut conn, new_id).unwrap().unwrap().face_count, 2);
    }

    /// `cluster_id = $3` in the split statement is what stops a stale id from
    /// the review screen dragging someone else's face across.
    #[test]
    fn split_ignores_faces_that_are_not_in_the_source_cluster() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, face_ids) = seed(&mut conn);
        let source = create(&mut conn, shoot_id, "Unknown Person 1").unwrap();
        let other = create(&mut conn, shoot_id, "Unknown Person 2").unwrap();
        faces::set_cluster(&mut conn, face_ids[0], Some(source)).unwrap();
        faces::set_cluster(&mut conn, face_ids[1], Some(other)).unwrap();
        refresh_counts(&mut conn, shoot_id).unwrap();

        let new_id = split(&mut conn, source, &[face_ids[0], face_ids[1]], "Unknown Person 3").unwrap();
        assert_eq!(get_by_id(&mut conn, new_id).unwrap().unwrap().face_count, 1);
        assert_eq!(
            faces::get_by_id(&mut conn, face_ids[1]).unwrap().unwrap().cluster_id,
            Some(other),
            "a face in another cluster is left where it is"
        );
    }

    #[test]
    fn clearing_unnamed_keeps_named_clusters() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, face_ids) = seed(&mut conn);
        let keep = create(&mut conn, shoot_id, "Unknown Person 1").unwrap();
        let drop = create(&mut conn, shoot_id, "Unknown Person 2").unwrap();
        faces::set_cluster(&mut conn, face_ids[0], Some(keep)).unwrap();
        let person = people::get_or_create(&mut conn, "Mavi", None).unwrap();
        name_cluster(&mut conn, keep, person.id).unwrap();

        clear_unnamed(&mut conn, shoot_id).unwrap();
        assert!(get_by_id(&mut conn, keep).unwrap().is_some());
        assert!(get_by_id(&mut conn, drop).unwrap().is_none());
    }
}
