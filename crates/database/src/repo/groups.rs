//! Manual groups — the folders an editor names and fills themselves.
//!
//! Unlike [`super::albums`], nothing in here is derived: a group exists because
//! a person created it and holds exactly the files a person put in it. That is
//! what makes it safe to export straight to a folder name. Membership rows are
//! pointers into `media`; removing one never touches a file on disk.

use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{Group, MediaGroupLink};
use crate::{now, params, DbError, Result};

fn map(row: &Row) -> Result<Group> {
    Ok(Group {
        id: get(row, "id")?,
        shoot_id: get(row, "shoot_id")?,
        name: get(row, "name")?,
        folder_name: get(row, "folder_name")?,
        notes: get(row, "notes")?,
        person_id: get(row, "person_id")?,
        sort_order: get(row, "sort_order")?,
        media_count: get(row, "media_count")?,
        photo_count: get(row, "photo_count")?,
        video_count: get(row, "video_count")?,
        cover_media_id: get(row, "cover_media_id")?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

/// A group name is going to become a folder name, so the rules are the ones a
/// filesystem cares about plus "a human can tell two groups apart".
fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DbError::other("give the group a name"));
    }
    if name.chars().count() > 120 {
        return Err(DbError::other("that group name is too long for a folder name"));
    }
    Ok(name.to_string())
}

pub fn get_by_id(conn: &mut dyn Db, id: i64) -> Result<Option<Group>> {
    conn.row_opt("SELECT * FROM media_groups WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

pub fn find_by_name(conn: &mut dyn Db, shoot_id: i64, name: &str) -> Result<Option<Group>> {
    // `media_groups.name` carries the `nocase` collation, so the comparison is
    // case-insensitive without the `COLLATE NOCASE` this used to spell out.
    conn.row_opt(
        "SELECT * FROM media_groups WHERE shoot_id = $1 AND name = $2",
        params![shoot_id, name.trim()],
    )?
    .as_ref()
    .map(map)
    .transpose()
}

/// Groups in the order the sorting screen shows them: the editor's own order
/// first, then alphabetical for everything they have not dragged around.
pub fn list(conn: &mut dyn Db, shoot_id: i64) -> Result<Vec<Group>> {
    conn.rows(
        "SELECT * FROM media_groups WHERE shoot_id = $1
          ORDER BY sort_order, name",
        params![shoot_id],
    )?
    .iter()
    .map(map)
    .collect()
}

/// Creates a group, or returns the existing one if that name is already taken
/// in this shoot. Naming the same group twice is a slip, not an error worth
/// interrupting a sorting session for.
pub fn get_or_create(conn: &mut dyn Db, shoot_id: i64, name: &str, person_id: Option<i64>) -> Result<Group> {
    let name = clean_name(name)?;
    let stamp = now();
    // One statement replaces find-then-insert. New groups still land at the end
    // of the list; the `DO UPDATE` is a deliberate no-op whose only job is to
    // make `RETURNING` produce the existing row when the name is taken.
    let row = conn.row_one(
        "INSERT INTO media_groups (shoot_id, name, person_id, sort_order, created_at, updated_at)
         VALUES ($1, $2, $3,
                 (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM media_groups WHERE shoot_id = $1),
                 $4, $4)
         ON CONFLICT (shoot_id, name) DO UPDATE SET updated_at = media_groups.updated_at
         RETURNING *",
        params![shoot_id, name, person_id, stamp],
    )?;
    map(&row)
}

pub fn rename(conn: &mut dyn Db, group_id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    // The unique index would reject this anyway; catching it here lets the
    // message name the actual problem.
    let existing: Option<i64> = conn
        .row_opt(
            "SELECT id FROM media_groups
              WHERE shoot_id = (SELECT shoot_id FROM media_groups WHERE id = $1)
                AND name = $2",
            params![group_id, name],
        )?
        .as_ref()
        .map(|r| super::at(r, 0))
        .transpose()?;
    if let Some(existing) = existing {
        if existing != group_id {
            return Err(DbError::other(format!(
                "this shoot already has a group called \"{name}\""
            )));
        }
    }

    conn.exec(
        "UPDATE media_groups SET name = $2, updated_at = $3 WHERE id = $1",
        params![group_id, name, now()],
    )?;
    Ok(())
}

/// `folder_name` of `None` (or blank) goes back to using the group's name.
pub fn update(conn: &mut dyn Db, group_id: i64, folder_name: Option<&str>, notes: Option<&str>) -> Result<()> {
    let folder = folder_name.map(str::trim).filter(|s| !s.is_empty());
    let notes = notes.map(str::trim).filter(|s| !s.is_empty());
    conn.exec(
        "UPDATE media_groups SET folder_name = $2, notes = $3, updated_at = $4 WHERE id = $1",
        params![group_id, folder, notes, now()],
    )?;
    Ok(())
}

/// Deletes the group and its membership. The media rows — and the files they
/// point at — are untouched.
pub fn delete(conn: &mut dyn Db, group_id: i64) -> Result<()> {
    conn.exec("DELETE FROM media_groups WHERE id = $1", params![group_id])?;
    Ok(())
}

/// The shoot a group belongs to, or an error naming the group as gone.
fn owning_shoot(conn: &mut dyn Db, group_id: i64) -> Result<i64> {
    conn.row_opt("SELECT shoot_id FROM media_groups WHERE id = $1", params![group_id])?
        .as_ref()
        .map(|r| super::at(r, 0))
        .transpose()?
        .ok_or_else(|| DbError::other("that group no longer exists"))
}

/// Adds files to a group, ignoring any that are already in it, and returns how
/// many were newly added.
///
/// Files from another shoot are rejected rather than silently dropped: it would
/// otherwise be possible to build a group whose export mixes two source
/// folders without the editor ever seeing why.
pub fn add_media(conn: &mut dyn Db, group_id: i64, media_ids: &[i64]) -> Result<usize> {
    if media_ids.is_empty() {
        return Ok(0);
    }
    let shoot_id = owning_shoot(conn, group_id)?;

    let stamp = now();
    // `INSERT … SELECT … ON CONFLICT DO NOTHING` over the whole selection,
    // instead of one statement per file. The `m.shoot_id = $4` join condition
    // is still what rejects another shoot's media — it simply selects no row.
    let added = conn.exec(
        "INSERT INTO media_group_items (group_id, media_id, added_at)
         SELECT $1::bigint, m.id, $3 FROM media m WHERE m.id = ANY($2) AND m.shoot_id = $4
         ON CONFLICT (group_id, media_id) DO NOTHING",
        params![group_id, media_ids, stamp, shoot_id],
    )?;

    refresh_counts(conn, group_id)?;
    Ok(added as usize)
}

pub fn remove_media(conn: &mut dyn Db, group_id: i64, media_ids: &[i64]) -> Result<usize> {
    if media_ids.is_empty() {
        return Ok(0);
    }
    let removed = conn.exec(
        "DELETE FROM media_group_items WHERE group_id = $1 AND media_id = ANY($2)",
        params![group_id, media_ids],
    )?;
    refresh_counts(conn, group_id)?;
    Ok(removed as usize)
}

/// Takes the files out of every group in the shoot and puts them in one — the
/// "move, don't copy" variant of [`add_media`], for the common case where a
/// file was filed under the wrong player.
pub fn move_media(conn: &mut dyn Db, group_id: i64, media_ids: &[i64]) -> Result<usize> {
    if media_ids.is_empty() {
        return Ok(0);
    }
    let shoot_id = owning_shoot(conn, group_id)?;

    let affected: Vec<i64> = conn
        .rows(
            "SELECT DISTINCT gi.group_id FROM media_group_items gi
               JOIN media_groups g ON g.id = gi.group_id
              WHERE g.shoot_id = $1",
            params![shoot_id],
        )?
        .iter()
        .map(|r| super::at(r, 0))
        .collect::<Result<Vec<_>>>()?;

    conn.exec(
        "DELETE FROM media_group_items
          WHERE media_id = ANY($1)
            AND group_id != $2
            AND group_id IN (SELECT id FROM media_groups WHERE shoot_id = $3)",
        params![media_ids, group_id, shoot_id],
    )?;

    let added = add_media(conn, group_id, media_ids)?;
    for other in affected.into_iter().filter(|id| *id != group_id) {
        refresh_counts(conn, other)?;
    }
    Ok(added)
}

pub fn clear(conn: &mut dyn Db, group_id: i64) -> Result<usize> {
    let removed = conn.exec("DELETE FROM media_group_items WHERE group_id = $1", params![group_id])?;
    refresh_counts(conn, group_id)?;
    Ok(removed as usize)
}

/// Media ids in a group, optionally narrowed to photos or videos, in the order
/// they will be written on export.
pub fn media_ids(conn: &mut dyn Db, group_id: i64, media_type: Option<&str>) -> Result<Vec<i64>> {
    conn.rows(
        "SELECT gi.media_id FROM media_group_items gi JOIN media m ON m.id = gi.media_id
          WHERE gi.group_id = $1 AND ($2::text IS NULL OR m.media_type = $2::text)
          ORDER BY (m.captured_at IS NULL), m.captured_at, m.filename COLLATE nocase",
        params![group_id, media_type],
    )?
    .iter()
    .map(|r| super::at(r, 0))
    .collect()
}

/// Which groups every grouped file in the shoot belongs to. The grid draws a
/// chip per group on each thumbnail from this, so it is one query rather than
/// one per tile.
pub fn links(conn: &mut dyn Db, shoot_id: i64) -> Result<Vec<MediaGroupLink>> {
    conn.rows(
        "SELECT gi.media_id, gi.group_id FROM media_group_items gi
           JOIN media_groups g ON g.id = gi.group_id
          WHERE g.shoot_id = $1",
        params![shoot_id],
    )?
    .iter()
    .map(|r| {
        Ok(MediaGroupLink {
            media_id: super::at(r, 0)?,
            group_id: super::at(r, 1)?,
        })
    })
    .collect()
}

/// Files in the shoot that are not in any group yet — the number the editor is
/// working down towards zero.
pub fn ungrouped_count(conn: &mut dyn Db, shoot_id: i64) -> Result<i64> {
    super::at(
        &conn.row_one(
            "SELECT COUNT(*) FROM media m
              WHERE m.shoot_id = $1
                AND NOT EXISTS (SELECT 1 FROM media_group_items gi WHERE gi.media_id = m.id)",
            params![shoot_id],
        )?,
        0,
    )
}

pub fn refresh_counts(conn: &mut dyn Db, group_id: i64) -> Result<i64> {
    // `RETURNING media_count` folds the follow-up read into the update.
    let row = conn.row_one(
        "UPDATE media_groups SET
             media_count = (SELECT COUNT(*) FROM media_group_items gi WHERE gi.group_id = $1),
             photo_count = (SELECT COUNT(*) FROM media_group_items gi JOIN media m ON m.id = gi.media_id
                             WHERE gi.group_id = $1 AND m.media_type = 'photo'),
             video_count = (SELECT COUNT(*) FROM media_group_items gi JOIN media m ON m.id = gi.media_id
                             WHERE gi.group_id = $1 AND m.media_type = 'video'),
             cover_media_id = (SELECT gi.media_id FROM media_group_items gi
                                 JOIN media m ON m.id = gi.media_id
                                WHERE gi.group_id = $1 AND m.thumbnail_path IS NOT NULL
                                ORDER BY gi.added_at, m.id LIMIT 1),
             updated_at = $2
          WHERE id = $1
      RETURNING media_count",
        params![group_id, now()],
    )?;
    super::at(&row, 0)
}

/// The head start: one group per player the AI identified, pre-filled with that
/// player's album. The editor then corrects instead of sorting from scratch.
///
/// Existing groups are added to rather than replaced, so running this again
/// after more faces are named tops the groups up without undoing any manual
/// edit. Returns (groups touched, files added).
pub fn seed_from_player_albums(conn: &mut dyn Db, shoot_id: i64) -> Result<(usize, usize)> {
    // `albums.name` is a plain `TEXT` column, so the collation is named inline.
    let players: Vec<(i64, String, Vec<i64>)> = conn
        .rows(
            "SELECT a.id, a.name, a.person_ids FROM albums a
              WHERE a.shoot_id = $1 AND a.album_type = 'player'
              ORDER BY a.media_count DESC, a.name COLLATE nocase",
            params![shoot_id],
        )?
        .iter()
        .map(|r| {
            let ids: Option<String> = super::at(r, 2)?;
            Ok((
                super::at::<i64>(r, 0)?,
                super::at::<String>(r, 1)?,
                ids.and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
                    .unwrap_or_default(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut groups_touched = 0usize;
    let mut files_added = 0usize;

    for (album_id, name, person_ids) in players {
        let group = get_or_create(conn, shoot_id, &name, person_ids.first().copied())?;
        let media = super::albums::media_ids(conn, album_id, None)?;
        let added = add_media(conn, group.id, &media)?;
        if added > 0 || group.media_count == 0 {
            groups_touched += 1;
        }
        files_added += added;
    }

    Ok((groups_touched, files_added))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaType, NewMedia};
    use crate::repo::{media, shoots};
    use crate::Database;

    fn seed(conn: &mut dyn Db) -> (i64, Vec<i64>) {
        let shoot = shoots::create(conn, "BGMS Finals", "C:\\raw").unwrap();
        let mut ids = Vec::new();
        for (i, name) in ["a.jpg", "b.jpg", "c.mp4"].iter().enumerate() {
            ids.push(
                media::upsert(
                    conn,
                    &NewMedia {
                        shoot_id: shoot.id,
                        path: format!("C:\\raw\\{name}"),
                        filename: name.to_string(),
                        media_type: if name.ends_with("mp4") {
                            MediaType::Video
                        } else {
                            MediaType::Photo
                        },
                        extension: name.split('.').next_back().unwrap().to_string(),
                        file_size: 10,
                        content_key: format!("k{i}"),
                        captured_at: None,
                    },
                )
                .unwrap(),
            );
        }
        (shoot.id, ids)
    }

    #[test]
    fn a_named_group_holds_exactly_what_was_put_in_it() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, media_ids) = seed(&mut conn);

        let group = get_or_create(&mut conn, shoot_id, "  Jonathan  ", None).unwrap();
        assert_eq!(group.name, "Jonathan", "the name is trimmed before it becomes a folder");

        assert_eq!(add_media(&mut conn, group.id, &media_ids[..2]).unwrap(), 2);
        // Adding the same file twice is a no-op, not a duplicate.
        assert_eq!(add_media(&mut conn, group.id, &media_ids[..2]).unwrap(), 0);

        let reloaded = get_by_id(&mut conn, group.id).unwrap().unwrap();
        assert_eq!(reloaded.media_count, 2);
        assert_eq!(reloaded.photo_count, 2);
        assert_eq!(reloaded.video_count, 0);
        assert_eq!(super::media_ids(&mut conn, group.id, None).unwrap().len(), 2);
    }

    #[test]
    fn one_file_can_sit_in_two_groups() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, media_ids) = seed(&mut conn);

        let a = get_or_create(&mut conn, shoot_id, "Jonathan", None).unwrap();
        let b = get_or_create(&mut conn, shoot_id, "Mavi", None).unwrap();
        add_media(&mut conn, a.id, &media_ids[..1]).unwrap();
        add_media(&mut conn, b.id, &media_ids[..1]).unwrap();

        assert_eq!(links(&mut conn, shoot_id).unwrap().len(), 2);
        assert_eq!(ungrouped_count(&mut conn, shoot_id).unwrap(), 2);
    }

    #[test]
    fn moving_a_file_takes_it_out_of_the_other_groups() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, media_ids) = seed(&mut conn);

        let wrong = get_or_create(&mut conn, shoot_id, "Wrong", None).unwrap();
        let right = get_or_create(&mut conn, shoot_id, "Right", None).unwrap();
        add_media(&mut conn, wrong.id, &media_ids).unwrap();

        move_media(&mut conn, right.id, &media_ids[..1]).unwrap();

        assert_eq!(get_by_id(&mut conn, wrong.id).unwrap().unwrap().media_count, 2);
        assert_eq!(get_by_id(&mut conn, right.id).unwrap().unwrap().media_count, 1);
    }

    #[test]
    fn a_duplicate_name_reuses_the_group_and_a_rename_onto_one_is_refused() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, _) = seed(&mut conn);

        let first = get_or_create(&mut conn, shoot_id, "Jonathan", None).unwrap();
        let again = get_or_create(&mut conn, shoot_id, "jonathan", None).unwrap();
        assert_eq!(first.id, again.id, "names are matched case-insensitively");
        assert_eq!(again.name, "Jonathan", "the original spelling is kept");

        let other = get_or_create(&mut conn, shoot_id, "Mavi", None).unwrap();
        assert!(rename(&mut conn, other.id, "Jonathan").is_err());
        assert!(
            rename(&mut conn, other.id, "Mavi ").is_ok(),
            "renaming to its own name is fine"
        );
    }

    #[test]
    fn an_empty_name_is_refused() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, _) = seed(&mut conn);
        assert!(get_or_create(&mut conn, shoot_id, "   ", None).is_err());
    }

    #[test]
    fn files_from_another_shoot_cannot_be_added() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, _) = seed(&mut conn);
        let (_, other_media) = seed(&mut conn);

        let group = get_or_create(&mut conn, shoot_id, "Jonathan", None).unwrap();
        assert_eq!(add_media(&mut conn, group.id, &other_media).unwrap(), 0);
    }

    #[test]
    fn deleting_a_group_leaves_the_media_index_alone() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, media_ids) = seed(&mut conn);

        let group = get_or_create(&mut conn, shoot_id, "Jonathan", None).unwrap();
        add_media(&mut conn, group.id, &media_ids).unwrap();
        delete(&mut conn, group.id).unwrap();

        assert!(list(&mut conn, shoot_id).unwrap().is_empty());
        assert_eq!(media::count_for_shoot(&mut conn, shoot_id).unwrap(), 3);
        assert_eq!(ungrouped_count(&mut conn, shoot_id).unwrap(), 3);
    }

    #[test]
    fn the_folder_name_falls_back_to_the_group_name() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, _) = seed(&mut conn);
        let group = get_or_create(&mut conn, shoot_id, "Jonathan", None).unwrap();

        update(&mut conn, group.id, Some("01_Jonathan"), None).unwrap();
        assert_eq!(
            get_by_id(&mut conn, group.id).unwrap().unwrap().folder_name.as_deref(),
            Some("01_Jonathan")
        );

        update(&mut conn, group.id, Some("   "), None).unwrap();
        assert_eq!(get_by_id(&mut conn, group.id).unwrap().unwrap().folder_name, None);
    }

    #[test]
    fn seeding_from_player_albums_tops_groups_up_without_undoing_edits() {
        use crate::models::{BoundingBox, NewFace};
        use crate::repo::{albums, faces, people};

        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, media_ids) = seed(&mut conn);

        let jonathan = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
        for media_id in &media_ids[..2] {
            let face_id = faces::insert(
                &mut conn,
                &NewFace {
                    media_id: *media_id,
                    shoot_id,
                    bbox: BoundingBox {
                        x: 0.0,
                        y: 0.0,
                        w: 0.2,
                        h: 0.2,
                    },
                    landmarks: None,
                    detection_confidence: 0.9,
                    embedding: Some(vec![1.0, 0.0]),
                    quality: Some(0.5),
                    frame_time: None,
                    crop_path: None,
                },
            )
            .unwrap();
            faces::assign(&mut conn, face_id, jonathan.id, Some(0.99)).unwrap();
        }
        albums::regenerate(&mut conn, shoot_id).unwrap();

        let (groups, files) = seed_from_player_albums(&mut conn, shoot_id).unwrap();
        assert_eq!((groups, files), (1, 2));

        // An editor's manual addition survives a second seeding run.
        let group = find_by_name(&mut conn, shoot_id, "Jonathan").unwrap().unwrap();
        add_media(&mut conn, group.id, &media_ids[2..]).unwrap();
        let (_, files_again) = seed_from_player_albums(&mut conn, shoot_id).unwrap();
        assert_eq!(files_again, 0);
        assert_eq!(get_by_id(&mut conn, group.id).unwrap().unwrap().media_count, 3);
    }

    #[test]
    fn a_named_video_sample_puts_the_complete_video_in_the_player_group() {
        use crate::models::{BoundingBox, NewFace};
        use crate::repo::{albums, faces, people};

        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let (shoot_id, source_ids) = seed(&mut conn);
        let video_id = source_ids[2];
        let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
        let face_id = faces::insert(
            &mut conn,
            &NewFace {
                media_id: video_id,
                shoot_id,
                bbox: BoundingBox {
                    x: 0.2,
                    y: 0.1,
                    w: 0.2,
                    h: 0.3,
                },
                landmarks: None,
                detection_confidence: 0.95,
                embedding: Some(vec![1.0, 0.0]),
                quality: Some(0.8),
                frame_time: Some(15.0),
                crop_path: None,
            },
        )
        .unwrap();
        faces::assign(&mut conn, face_id, person.id, Some(1.0)).unwrap();

        albums::regenerate(&mut conn, shoot_id).unwrap();
        let (_, files) = seed_from_player_albums(&mut conn, shoot_id).unwrap();
        let group = find_by_name(&mut conn, shoot_id, "Jonathan").unwrap().unwrap();

        assert_eq!(files, 1);
        assert_eq!(group.media_count, 1);
        assert_eq!(group.photo_count, 0);
        assert_eq!(group.video_count, 1);
        assert_eq!(media_ids(&mut conn, group.id, Some("video")).unwrap(), vec![video_id]);
    }
}
