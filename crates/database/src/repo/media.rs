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
        person_count: get(row, "person_count")?,
        quality_score: get(row, "quality_score")?,
        sharpness_score: get(row, "sharpness_score")?,
        exposure_score: get(row, "exposure_score")?,
        perceptual_hash: get(row, "perceptual_hash")?,
        duplicate_group_id: get(row, "duplicate_group_id")?,
        duplicate_count: get(row, "duplicate_count")?,
        is_best_shot: get::<i64>(row, "is_best_shot")? != 0,
        rating: get(row, "rating")?,
        pick_state: get(row, "pick_state")?,
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
              quality_score     = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.quality_score ELSE NULL END,
              sharpness_score   = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.sharpness_score ELSE NULL END,
              exposure_score    = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.exposure_score ELSE NULL END,
              perceptual_hash   = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.perceptual_hash ELSE NULL END,
              duplicate_group_id = CASE WHEN media.content_key = excluded.content_key
                                         THEN media.duplicate_group_id ELSE NULL END,
              duplicate_count   = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.duplicate_count ELSE 1 END,
              is_best_shot      = CASE WHEN media.content_key = excluded.content_key
                                       THEN media.is_best_shot ELSE 0 END,
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

/// Applies an editor's rating and/or pick decision to one or more files.
/// Passing `None` for a field leaves that field unchanged.
pub fn set_editorial_state(
    conn: &Connection,
    media_ids: &[i64],
    rating: Option<i64>,
    pick_state: Option<&str>,
) -> Result<usize> {
    if rating.is_none() && pick_state.is_none() {
        return Ok(0);
    }
    if let Some(value) = rating {
        if !(0..=5).contains(&value) {
            return Err(crate::DbError::other("rating must be between 0 and 5"));
        }
    }
    if let Some(value) = pick_state {
        if !matches!(value, "none" | "pick" | "reject") {
            return Err(crate::DbError::other("pick state must be none, pick, or reject"));
        }
    }

    let mut changed = 0;
    let mut stmt = conn.prepare(
        "UPDATE media
            SET rating = COALESCE(?2, rating),
                pick_state = COALESCE(?3, pick_state)
          WHERE id = ?1",
    )?;
    for media_id in media_ids {
        changed += stmt.execute(params![media_id, rating, pick_state])?;
    }
    Ok(changed)
}

pub fn set_quality(
    conn: &Connection,
    id: i64,
    quality: f64,
    sharpness: f64,
    exposure: f64,
    perceptual_hash: u64,
) -> Result<()> {
    conn.execute(
        "UPDATE media
            SET quality_score = ?2, sharpness_score = ?3, exposure_score = ?4,
                perceptual_hash = ?5, is_best_shot = 1
          WHERE id = ?1",
        params![id, quality, sharpness, exposure, format!("{perceptual_hash:016x}")],
    )?;
    Ok(())
}

/// Groups visually similar photos and promotes the best-scoring member.
///
/// A 64-bit difference hash is intentionally conservative here: a Hamming
/// distance of six catches resized/re-encoded bursts without treating broadly
/// similar compositions as copies. Single photos remain best shots but do not
/// receive a duplicate-group badge.
pub fn refresh_duplicate_groups(conn: &Connection, shoot_id: i64, max_distance: u32) -> Result<usize> {
    let candidates = {
        let mut stmt = conn.prepare(
            "SELECT id, perceptual_hash, COALESCE(quality_score, 0)
               FROM media
              WHERE shoot_id = ?1 AND media_type = 'photo' AND perceptual_hash IS NOT NULL
              ORDER BY id",
        )?;
        let rows = stmt.query_map(params![shoot_id], |row| {
            let encoded: String = row.get(1)?;
            Ok((
                row.get::<_, i64>(0)?,
                u64::from_str_radix(&encoded, 16).unwrap_or_default(),
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    conn.execute(
        "UPDATE media
            SET duplicate_group_id = NULL, duplicate_count = 1,
                is_best_shot = CASE WHEN quality_score IS NULL THEN 0 ELSE 1 END
          WHERE shoot_id = ?1 AND media_type = 'photo'",
        params![shoot_id],
    )?;

    let mut parent: Vec<usize> = (0..candidates.len()).collect();
    // Split the hash into distance+1 bands. If two hashes differ in at most D
    // bits, at least one of D+1 bands must be identical. This avoids comparing
    // every photo with every other photo on normal large shoots.
    let band_count = max_distance.clamp(1, 63) + 1;
    let mut buckets = std::collections::HashMap::<(u32, u64), Vec<usize>>::new();
    let mut compared = std::collections::HashSet::<(usize, usize)>::new();
    for right in 0..candidates.len() {
        for band in 0..band_count {
            let key = (band, hash_band(candidates[right].1, band, band_count));
            if let Some(lefts) = buckets.get(&key) {
                for &left in lefts {
                    if compared.insert((left, right))
                        && (candidates[left].1 ^ candidates[right].1).count_ones() <= max_distance
                    {
                        union(&mut parent, left, right);
                    }
                }
            }
            buckets.entry(key).or_default().push(right);
        }
    }

    let mut groups = std::collections::BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..candidates.len() {
        let root = find(&mut parent, index);
        groups.entry(root).or_default().push(index);
    }

    let mut duplicate_groups = 0;
    for members in groups.values().filter(|members| members.len() > 1) {
        duplicate_groups += 1;
        let best = members
            .iter()
            .copied()
            .max_by(|&a, &b| {
                candidates[a]
                    .2
                    .total_cmp(&candidates[b].2)
                    .then_with(|| candidates[b].0.cmp(&candidates[a].0))
            })
            .unwrap_or(members[0]);
        let group_id = candidates[members[0]].0;
        for &member in members {
            conn.execute(
                "UPDATE media
                    SET duplicate_group_id = ?2, duplicate_count = ?3, is_best_shot = ?4
                  WHERE id = ?1",
                params![
                    candidates[member].0,
                    group_id,
                    members.len() as i64,
                    i64::from(member == best),
                ],
            )?;
        }
    }

    Ok(duplicate_groups)
}

fn find(parent: &mut [usize], node: usize) -> usize {
    if parent[node] != node {
        parent[node] = find(parent, parent[node]);
    }
    parent[node]
}

fn union(parent: &mut [usize], left: usize, right: usize) {
    let left_root = find(parent, left);
    let right_root = find(parent, right);
    if left_root != right_root {
        parent[right_root] = left_root;
    }
}

fn hash_band(hash: u64, band: u32, band_count: u32) -> u64 {
    let start = band * 64 / band_count;
    let end = (band + 1) * 64 / band_count;
    let width = end - start;
    let mask = if width == 64 { u64::MAX } else { (1_u64 << width) - 1 };
    (hash >> start) & mask
}

pub fn set_status(conn: &Connection, id: i64, status: ProcessingStatus, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE media SET processing_status = ?2, error = ?3 WHERE id = ?1",
        params![id, status, error],
    )?;
    Ok(())
}

/// Corrects orientation when analysis defensively re-reads the source file.
/// Indexing normally writes this first; keeping the repair narrow avoids
/// replacing unrelated metadata with fallbacks after a transient read issue.
pub fn set_orientation(conn: &Connection, id: i64, orientation: i64) -> Result<()> {
    conn.execute(
        "UPDATE media SET orientation = ?2 WHERE id = ?1",
        params![id, orientation.clamp(1, 8)],
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

/// Recomputes how many distinct *people* are in each file of a shoot.
///
/// `face_count` cannot answer this: video analysis stores one face row per
/// detection per sampled frame, so a one-person interview sampled twenty times
/// has twenty rows. The count here is
///
/// ```text
/// max( distinct identities + most unidentified faces in one frame,
///      most faces visible in any one frame )
/// ```
///
/// An identity is `person_id`, or `cluster_id` when the face is grouped but not
/// yet named. Each term earns its place:
///
/// * *distinct identities* is what makes a clip of one player, sampled twenty
///   times, count as one person rather than twenty.
/// * *most unidentified faces in one frame* estimates how many distinct
///   strangers there are before clustering has had a chance to group them.
/// * *most faces in one frame* is a floor. Two faces in a single frame are two
///   people — nobody appears twice in one photograph — so when clustering
///   wrongly merges two of them, this stops the count sagging below reality.
///
/// The expression needs no branching on media type: a photo's `frame_time` is
/// NULL, so all its faces fall into one group and it collapses to "faces in
/// the frame".
pub fn refresh_person_counts(conn: &Connection, shoot_id: i64) -> Result<()> {
    conn.execute(
        "WITH per_frame AS (
             SELECT media_id, frame_time,
                    COUNT(*) AS total,
                    SUM(CASE WHEN person_id IS NULL AND cluster_id IS NULL THEN 1 ELSE 0 END) AS unknown
               FROM faces
              WHERE shoot_id = ?1 AND assignment != 'ignored'
              GROUP BY media_id, frame_time
         ),
         frame_max AS (
             SELECT media_id, MAX(total) AS max_total, MAX(unknown) AS max_unknown
               FROM per_frame GROUP BY media_id
         ),
         identified AS (
             SELECT media_id,
                    COUNT(DISTINCT CASE WHEN person_id  IS NOT NULL THEN 'p' || person_id
                                        WHEN cluster_id IS NOT NULL THEN 'c' || cluster_id END) AS c
               FROM faces
              WHERE shoot_id = ?1 AND assignment != 'ignored'
              GROUP BY media_id
         )
         UPDATE media SET person_count = MAX(
               COALESCE((SELECT c           FROM identified WHERE media_id = media.id), 0)
             + COALESCE((SELECT max_unknown FROM frame_max  WHERE media_id = media.id), 0),
               COALESCE((SELECT max_total   FROM frame_max  WHERE media_id = media.id), 0)
         )
          WHERE shoot_id = ?1",
        params![shoot_id],
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
    if q.group_id.is_some() {
        sql.push_str(" JOIN media_group_items gi ON gi.media_id = m.id");
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
    if let Some(group_id) = q.group_id {
        wheres.push(format!("gi.group_id = ?{}", args.len() + 1));
        args.push(Box::new(group_id));
    }
    if q.ungrouped {
        wheres.push("NOT EXISTS (SELECT 1 FROM media_group_items x WHERE x.media_id = m.id)".to_string());
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
    if let Some(size) = q.group_size {
        // At the cap this means "or more", so the filter matches the album it
        // came from (see repo::albums::GROUP_SIZE_CAP).
        if size >= crate::repo::albums::GROUP_SIZE_CAP {
            wheres.push(format!("m.person_count >= ?{}", args.len() + 1));
        } else {
            wheres.push(format!("m.person_count = ?{}", args.len() + 1));
        }
        args.push(Box::new(size));
    }
    if let Some(media_type) = &q.media_type {
        wheres.push(format!("m.media_type = ?{}", args.len() + 1));
        args.push(Box::new(media_type.clone()));
    }
    if q.only_best_shots {
        wheres.push("m.media_type = 'photo' AND m.is_best_shot = 1".to_string());
    }
    if q.only_duplicates {
        wheres.push("m.media_type = 'photo' AND m.duplicate_group_id IS NOT NULL".to_string());
    }
    if let Some(min_rating) = q.min_rating.filter(|value| *value > 0) {
        wheres.push(format!("m.rating >= ?{}", args.len() + 1));
        args.push(Box::new(min_rating.clamp(1, 5)));
    }
    if let Some(pick_state) = q.pick_state.as_deref() {
        if matches!(pick_state, "none" | "pick" | "reject") {
            wheres.push(format!("m.pick_state = ?{}", args.len() + 1));
            args.push(Box::new(pick_state.to_string()));
        }
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
    match q.sort.as_deref() {
        Some("quality") => sql.push_str(" ORDER BY m.quality_score IS NULL, m.quality_score DESC, m.filename"),
        Some("rating") => sql.push_str(
            " ORDER BY m.rating DESC, (m.pick_state = 'pick') DESC, m.captured_at IS NULL, m.captured_at, m.filename",
        ),
        Some("filename") => sql.push_str(" ORDER BY m.filename COLLATE NOCASE"),
        _ => sql.push_str(" ORDER BY m.captured_at IS NULL, m.captured_at, m.filename"),
    }

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
        for (i, name) in ["a.jpg", "b.raf", "c.mp4"].iter().enumerate() {
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
    fn query_filters_by_group_and_backlog() {
        use crate::repo::groups;

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);
        let all = query(
            &conn,
            &MediaQuery {
                shoot_id: Some(shoot_id),
                ..Default::default()
            },
        )
        .unwrap();

        let group = groups::get_or_create(&conn, shoot_id, "Jonathan", None).unwrap();
        groups::add_media(&conn, group.id, &[all[0].id]).unwrap();

        let in_group = query(
            &conn,
            &MediaQuery {
                shoot_id: Some(shoot_id),
                group_id: Some(group.id),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(in_group.len(), 1);

        let backlog = query(
            &conn,
            &MediaQuery {
                shoot_id: Some(shoot_id),
                ungrouped: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(backlog.len(), all.len() - 1);
        assert!(!backlog.iter().any(|m| m.id == all[0].id));
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

    #[test]
    fn near_duplicates_promote_the_highest_quality_photo() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);
        let a = conn
            .query_row("SELECT id FROM media WHERE filename = 'a.jpg'", [], |row| row.get(0))
            .unwrap();
        let b = conn
            .query_row("SELECT id FROM media WHERE filename = 'b.raf'", [], |row| row.get(0))
            .unwrap();

        set_quality(&conn, a, 0.45, 0.4, 0.6, 0xaaaa_aaaa_aaaa_aaaa).unwrap();
        set_quality(&conn, b, 0.90, 0.9, 0.9, 0xaaaa_aaaa_aaaa_aaab).unwrap();
        assert_eq!(refresh_duplicate_groups(&conn, shoot_id, 6).unwrap(), 1);

        let first = get_by_id(&conn, a).unwrap().unwrap();
        let second = get_by_id(&conn, b).unwrap().unwrap();
        assert_eq!(first.duplicate_group_id, second.duplicate_group_id);
        assert_eq!(first.duplicate_count, 2);
        assert!(!first.is_best_shot);
        assert!(second.is_best_shot);

        let best = query(
            &conn,
            &MediaQuery {
                shoot_id: Some(shoot_id),
                only_best_shots: true,
                sort: Some("quality".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(best.iter().map(|item| item.id).collect::<Vec<_>>(), vec![b]);
    }

    #[test]
    fn editorial_ratings_persist_filter_and_sort() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot_id = seed(&conn);
        let all = query(
            &conn,
            &MediaQuery { shoot_id: Some(shoot_id), ..Default::default() },
        )
        .unwrap();

        set_editorial_state(&conn, &[all[0].id], Some(3), Some("pick")).unwrap();
        set_editorial_state(&conn, &[all[1].id], Some(5), None).unwrap();

        let picks = query(
            &conn,
            &MediaQuery {
                shoot_id: Some(shoot_id),
                pick_state: Some("pick".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].rating, 3);

        let rated = query(
            &conn,
            &MediaQuery {
                shoot_id: Some(shoot_id),
                min_rating: Some(3),
                sort: Some("rating".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rated.iter().map(|item| item.rating).collect::<Vec<_>>(), vec![5, 3]);

        reset_analysis(&conn, shoot_id).unwrap();
        let kept = get_by_id(&conn, all[0].id).unwrap().unwrap();
        assert_eq!(kept.rating, 3);
        assert_eq!(kept.pick_state, "pick");
    }
}
