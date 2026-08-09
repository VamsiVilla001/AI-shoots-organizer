//! Album generation and lookup.
//!
//! Albums are *derived* state: everything here can be rebuilt from `faces` at
//! any time, which is why [`regenerate`] simply drops and rewrites a shoot's
//! albums rather than trying to patch them incrementally.

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{Album, AlbumType};
use crate::{now, Result};

/// How many co-occurrence pairs to keep. Every player pairs with every other
/// player they share a frame with, so an unbounded list would drown the useful
/// ones on a large team shoot.
const MAX_MULTI_PLAYER_ALBUMS: usize = 60;

/// Pairs seen fewer times than this are incidental (someone in the background)
/// rather than a real "these two together" set.
const MIN_MULTI_PLAYER_MEDIA: i64 = 2;

fn map(row: &Row<'_>) -> rusqlite::Result<Album> {
    let person_ids: Option<String> = get(row, "person_ids")?;
    Ok(Album {
        id: get(row, "id")?,
        shoot_id: get(row, "shoot_id")?,
        name: get(row, "name")?,
        album_type: get(row, "album_type")?,
        person_ids: person_ids
            .and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
            .unwrap_or_default(),
        cluster_id: get(row, "cluster_id")?,
        cover_media_id: get(row, "cover_media_id")?,
        media_count: get(row, "media_count")?,
        photo_count: get(row, "photo_count")?,
        video_count: get(row, "video_count")?,
        sort_order: get(row, "sort_order")?,
        generated_at: get(row, "generated_at")?,
    })
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<Album>> {
    Ok(conn
        .prepare("SELECT * FROM albums WHERE id = ?1")?
        .query_row(params![id], map)
        .optional()?)
}

pub fn list(conn: &Connection, shoot_id: i64) -> Result<Vec<Album>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM albums WHERE shoot_id = ?1
          ORDER BY CASE album_type
                     WHEN 'player'       THEN 0
                     WHEN 'multiPlayer'  THEN 1
                     WHEN 'team'         THEN 2
                     ELSE 3
                   END,
                   media_count DESC, name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![shoot_id], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn insert_album(
    conn: &Connection,
    shoot_id: i64,
    name: &str,
    album_type: AlbumType,
    person_ids: &[i64],
    cluster_id: Option<i64>,
    sort_order: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO albums (shoot_id, name, album_type, person_ids, cluster_id, sort_order, generated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            shoot_id,
            name,
            album_type,
            serde_json::to_string(person_ids)?,
            cluster_id,
            sort_order,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn refresh_album_counts(conn: &Connection, album_id: i64) -> Result<i64> {
    conn.execute(
        "UPDATE albums SET
             media_count = (SELECT COUNT(*) FROM album_media am WHERE am.album_id = ?1),
             photo_count = (SELECT COUNT(*) FROM album_media am JOIN media m ON m.id = am.media_id
                             WHERE am.album_id = ?1 AND m.media_type = 'photo'),
             video_count = (SELECT COUNT(*) FROM album_media am JOIN media m ON m.id = am.media_id
                             WHERE am.album_id = ?1 AND m.media_type = 'video'),
             cover_media_id = (SELECT am.media_id FROM album_media am
                                 JOIN media m ON m.id = am.media_id
                                WHERE am.album_id = ?1 AND m.thumbnail_path IS NOT NULL
                                ORDER BY m.id LIMIT 1)
          WHERE id = ?1",
        params![album_id],
    )?;
    Ok(conn.query_row("SELECT media_count FROM albums WHERE id = ?1", params![album_id], |r| {
        r.get(0)
    })?)
}

/// Rebuilds every album for a shoot from the current face assignments.
/// Returns the number of albums produced.
///
/// Run this inside a transaction — it deletes before it writes.
pub fn regenerate(conn: &Connection, shoot_id: i64) -> Result<usize> {
    conn.execute("DELETE FROM albums WHERE shoot_id = ?1", params![shoot_id])?;
    let mut created = 0usize;

    // ---- Player albums -----------------------------------------------------
    // One per identified player, holding every file they appear in. A file with
    // three players lands in three albums, exactly as §5 requires.
    let players: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT p.id, p.name, p.team
               FROM faces f JOIN people p ON p.id = f.person_id
              WHERE f.shoot_id = ?1 AND f.assignment IN ('suggested','confirmed')
              ORDER BY p.name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map(params![shoot_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    for (order, (person_id, name, _)) in players.iter().enumerate() {
        let album_id = insert_album(conn, shoot_id, name, AlbumType::Player, &[*person_id], None, order as i64)?;
        conn.execute(
            "INSERT OR IGNORE INTO album_media (album_id, media_id)
             SELECT DISTINCT ?1, f.media_id FROM faces f
              WHERE f.shoot_id = ?2 AND f.person_id = ?3 AND f.assignment IN ('suggested','confirmed')",
            params![album_id, shoot_id, person_id],
        )?;
        if refresh_album_counts(conn, album_id)? == 0 {
            conn.execute("DELETE FROM albums WHERE id = ?1", params![album_id])?;
        } else {
            created += 1;
        }
    }

    // ---- Multi-player albums ----------------------------------------------
    // Co-occurrence pairs, most frequent first (§8).
    let pairs: Vec<(i64, i64, String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT a.person_id AS a_id, b.person_id AS b_id, pa.name AS a_name, pb.name AS b_name,
                    COUNT(DISTINCT a.media_id) AS shared
               FROM faces a
               JOIN faces b  ON b.media_id = a.media_id AND b.person_id > a.person_id
               JOIN people pa ON pa.id = a.person_id
               JOIN people pb ON pb.id = b.person_id
              WHERE a.shoot_id = ?1
                AND a.assignment IN ('suggested','confirmed')
                AND b.assignment IN ('suggested','confirmed')
              GROUP BY a.person_id, b.person_id
             HAVING shared >= ?2
              ORDER BY shared DESC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![shoot_id, MIN_MULTI_PLAYER_MEDIA, MAX_MULTI_PLAYER_ALBUMS as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    for (order, (a_id, b_id, a_name, b_name, _)) in pairs.iter().enumerate() {
        let name = format!("{a_name} + {b_name}");
        let album_id = insert_album(
            conn,
            shoot_id,
            &name,
            AlbumType::MultiPlayer,
            &[*a_id, *b_id],
            None,
            order as i64,
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO album_media (album_id, media_id)
             SELECT ?1, a.media_id FROM faces a JOIN faces b ON b.media_id = a.media_id
              WHERE a.shoot_id = ?2 AND a.person_id = ?3 AND b.person_id = ?4
                AND a.assignment IN ('suggested','confirmed') AND b.assignment IN ('suggested','confirmed')
              GROUP BY a.media_id",
            params![album_id, shoot_id, a_id, b_id],
        )?;
        if refresh_album_counts(conn, album_id)? == 0 {
            conn.execute("DELETE FROM albums WHERE id = ?1", params![album_id])?;
        } else {
            created += 1;
        }
    }

    // ---- Team albums (optional, §8) ---------------------------------------
    let teams: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT p.team FROM faces f JOIN people p ON p.id = f.person_id
              WHERE f.shoot_id = ?1 AND p.team IS NOT NULL AND TRIM(p.team) != ''
                AND f.assignment IN ('suggested','confirmed')
              ORDER BY p.team COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map(params![shoot_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    for (order, team) in teams.iter().enumerate() {
        let member_ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM people WHERE team = ?1")?;
            let rows = stmt
                .query_map(params![team], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let album_id = insert_album(conn, shoot_id, team, AlbumType::Team, &member_ids, None, order as i64)?;
        conn.execute(
            "INSERT OR IGNORE INTO album_media (album_id, media_id)
             SELECT ?1, f.media_id FROM faces f JOIN people p ON p.id = f.person_id
              WHERE f.shoot_id = ?2 AND p.team = ?3 AND f.assignment IN ('suggested','confirmed')
              GROUP BY f.media_id",
            params![album_id, shoot_id, team],
        )?;
        if refresh_album_counts(conn, album_id)? == 0 {
            conn.execute("DELETE FROM albums WHERE id = ?1", params![album_id])?;
        } else {
            created += 1;
        }
    }

    // ---- Unidentified ------------------------------------------------------
    // Everything still holding a face nobody has claimed.
    let album_id = insert_album(conn, shoot_id, "Unidentified", AlbumType::Unidentified, &[], None, 0)?;
    conn.execute(
        "INSERT OR IGNORE INTO album_media (album_id, media_id)
         SELECT ?1, f.media_id FROM faces f
          WHERE f.shoot_id = ?2 AND f.person_id IS NULL AND f.assignment NOT IN ('ignored')
          GROUP BY f.media_id",
        params![album_id, shoot_id],
    )?;
    if refresh_album_counts(conn, album_id)? == 0 {
        conn.execute("DELETE FROM albums WHERE id = ?1", params![album_id])?;
    } else {
        created += 1;
    }

    Ok(created)
}

/// Media ids in an album, optionally narrowed to photos or videos — this is
/// what backs the "Photos" and "Videos" filters in §8.
pub fn media_ids(conn: &Connection, album_id: i64, media_type: Option<&str>) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT am.media_id FROM album_media am JOIN media m ON m.id = am.media_id
          WHERE am.album_id = ?1 AND (?2 IS NULL OR m.media_type = ?2)
          ORDER BY m.captured_at IS NULL, m.captured_at, m.filename",
    )?;
    let rows = stmt
        .query_map(params![album_id, media_type], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BoundingBox, MediaType, NewFace, NewMedia};
    use crate::repo::{faces, media, people, shoots};
    use crate::Database;

    /// Builds a shoot where Jonathan is in images 0-2, Mavi in 1-3.
    fn seed(conn: &Connection) -> i64 {
        let shoot = shoots::create(conn, "BGMS Finals", "C:\\s").unwrap();
        let jonathan = people::get_or_create(conn, "Jonathan", Some("Gods Reign")).unwrap();
        let mavi = people::get_or_create(conn, "Mavi", Some("Gods Reign")).unwrap();

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
                },
            )
            .unwrap();

            let mut people_here = Vec::new();
            if i < 3 {
                people_here.push(jonathan.id);
            }
            if i >= 1 {
                people_here.push(mavi.id);
            }
            for person_id in people_here {
                let face_id = faces::insert(
                    conn,
                    &NewFace {
                        media_id,
                        shoot_id: shoot.id,
                        bbox: BoundingBox { x: 0.0, y: 0.0, w: 0.1, h: 0.1 },
                        landmarks: None,
                        detection_confidence: 0.95,
                        embedding: Some(vec![1.0, 0.0]),
                        quality: Some(0.5),
                        frame_time: None,
                        crop_path: None,
                    },
                )
                .unwrap();
                faces::assign(conn, face_id, person_id, Some(0.99)).unwrap();
            }
        }
        shoot.id
    }

    #[test]
    fn generates_player_and_multi_player_albums() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);

        regenerate(&conn, shoot_id).unwrap();
        let albums = list(&conn, shoot_id).unwrap();

        let jonathan = albums.iter().find(|a| a.name == "Jonathan").expect("player album");
        assert_eq!(jonathan.media_count, 3);
        assert_eq!(jonathan.photo_count, 3);

        let mavi = albums.iter().find(|a| a.name == "Mavi").expect("player album");
        assert_eq!(mavi.media_count, 3);

        // Images 1 and 2 hold both players.
        let together = albums
            .iter()
            .find(|a| a.album_type == "multiPlayer")
            .expect("multi-player album");
        assert_eq!(together.media_count, 2);
        assert_eq!(together.person_ids.len(), 2);

        // Both players share a team, so a team album exists too.
        assert!(albums.iter().any(|a| a.album_type == "team" && a.name == "Gods Reign"));

        // Everyone is identified, so there is nothing unidentified to show.
        assert!(!albums.iter().any(|a| a.album_type == "unidentified"));
    }

    #[test]
    fn regenerating_is_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);

        let first = regenerate(&conn, shoot_id).unwrap();
        let second = regenerate(&conn, shoot_id).unwrap();
        assert_eq!(first, second);
        assert_eq!(list(&conn, shoot_id).unwrap().len(), first);
    }

    #[test]
    fn unidentified_album_collects_unclaimed_faces() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);

        let media_id = media::upsert(
            &conn,
            &NewMedia {
                shoot_id,
                path: "C:\\s\\stranger.jpg".into(),
                filename: "stranger.jpg".into(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: "kx".into(),
                captured_at: None,
            },
        )
        .unwrap();
        faces::insert(
            &conn,
            &NewFace {
                media_id,
                shoot_id,
                bbox: BoundingBox { x: 0.0, y: 0.0, w: 0.1, h: 0.1 },
                landmarks: None,
                detection_confidence: 0.91,
                embedding: Some(vec![0.0, 1.0]),
                quality: Some(0.5),
                frame_time: None,
                crop_path: None,
            },
        )
        .unwrap();

        regenerate(&conn, shoot_id).unwrap();
        let unidentified = list(&conn, shoot_id)
            .unwrap()
            .into_iter()
            .find(|a| a.album_type == "unidentified")
            .expect("unidentified album");
        assert_eq!(unidentified.media_count, 1);
    }
}
