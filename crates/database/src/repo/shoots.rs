use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{Shoot, ShootStatus, ShootSummary};
use crate::{now, params, Result};

fn map(row: &Row) -> Result<Shoot> {
    Ok(Shoot {
        id: get(row, "id")?,
        name: get(row, "name")?,
        source_path: get(row, "source_path")?,
        share_path: get(row, "share_path")?,
        status: get(row, "status")?,
        notes: get(row, "notes")?,
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

pub fn create(conn: &mut dyn Db, name: &str, source_path: &str) -> Result<Shoot> {
    let ts = now();
    let row = conn.row_one(
        "INSERT INTO shoots (name, source_path, status, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $4)
         RETURNING *",
        params![name, source_path, ShootStatus::Created.as_str(), ts],
    )?;
    map(&row)
}

pub fn get_by_id(conn: &mut dyn Db, id: i64) -> Result<Option<Shoot>> {
    conn.row_opt("SELECT * FROM shoots WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

pub fn list(conn: &mut dyn Db) -> Result<Vec<Shoot>> {
    conn.rows(
        "SELECT * FROM shoots WHERE is_reference = 0 ORDER BY created_at DESC, id DESC",
        params![],
    )?
    .iter()
    .map(map)
    .collect()
}

/// The single hidden shoot that parks person-enrollment reference photos and
/// videos. It never appears in the Media library / Processed jobs listings
/// (§ `list`/`list_summaries` both filter `is_reference = 0`), because faces
/// and media rows require a non-null `shoot_id` and reference material is not
/// part of any real import.
pub fn get_or_create_reference_library(conn: &mut dyn Db) -> Result<Shoot> {
    if let Some(row) = conn.row_opt("SELECT * FROM shoots WHERE is_reference = 1 LIMIT 1", params![])? {
        return map(&row);
    }

    let ts = now();
    // `ON CONFLICT` cannot help here (there is no unique index on
    // `is_reference`), so two workers racing could in principle both insert.
    // In practice this is only ever reached from the enrollment screen on the
    // UI thread; the `SELECT` above makes the repeat call cheap.
    let row = conn.row_one(
        "INSERT INTO shoots (name, source_path, status, is_reference, created_at, updated_at)
         VALUES ('Reference Library', '', $1, 1, $2, $2)
         RETURNING *",
        params![ShootStatus::Completed.as_str(), ts],
    )?;
    map(&row)
}

/// A read-only lookup for callers that must not create the reference shoot as
/// a side effect of merely checking whether anyone has enrolled yet.
pub fn reference_library_id(conn: &mut dyn Db) -> Result<Option<i64>> {
    conn.row_opt("SELECT id FROM shoots WHERE is_reference = 1 LIMIT 1", params![])?
        .as_ref()
        .map(|row| get(row, "id"))
        .transpose()
}

/// The Shoots screen listing: one row per shoot with its counts already rolled
/// up, so the UI never has to fan out N+1 queries.
pub fn list_summaries(conn: &mut dyn Db) -> Result<Vec<ShootSummary>> {
    // `julianday(a) - julianday(b)` became an interval subtraction. The `*_at`
    // columns are RFC3339 text, so they are cast to `timestamptz` for the
    // arithmetic and `MAX(0, …)` became `GREATEST` — in Postgres `MAX` is only
    // ever the aggregate.
    conn.rows(
        "SELECT s.*,
                (SELECT COUNT(*) FROM media m WHERE m.shoot_id = s.id AND m.media_type = 'photo')  AS photo_count,
                (SELECT COUNT(*) FROM media m WHERE m.shoot_id = s.id AND m.media_type = 'video')  AS video_count,
                (SELECT COUNT(*) FROM faces f WHERE f.shoot_id = s.id)                             AS face_count,
                (SELECT COUNT(DISTINCT f.person_id) FROM faces f
                   WHERE f.shoot_id = s.id AND f.person_id IS NOT NULL
                     AND f.assignment IN ('suggested', 'confirmed'))                               AS person_count,
                (SELECT COUNT(*) FROM clusters c WHERE c.shoot_id = s.id AND c.status = 'unnamed') AS unknown_cluster_count,
                (SELECT COUNT(*) FROM jobs j WHERE j.shoot_id = s.id AND j.state IN ('queued','running')) AS pending_jobs,
                (SELECT COUNT(*) FROM jobs j WHERE j.shoot_id = s.id AND j.state = 'failed')       AS failed_jobs,
                pr.started_at AS processing_started_at,
                pr.scan_completed_at AS scan_completed_at,
                pr.completed_at AS processing_completed_at,
                CASE WHEN pr.started_at IS NULL THEN NULL ELSE
                  GREATEST(0, EXTRACT(EPOCH FROM (
                      COALESCE(pr.completed_at::timestamptz, now()) - pr.started_at::timestamptz
                  )) * 1000)::bigint
                END AS processing_duration_ms
           FROM shoots s
           LEFT JOIN processing_runs pr ON pr.id = (
                SELECT r.id FROM processing_runs r
                 WHERE r.shoot_id = s.id ORDER BY r.started_at DESC, r.id DESC LIMIT 1
           )
          WHERE s.is_reference = 0
          ORDER BY s.created_at DESC, s.id DESC",
        params![],
    )?
    .iter()
    .map(|row| {
        Ok(ShootSummary {
            shoot: map(row)?,
            photo_count: get(row, "photo_count")?,
            video_count: get(row, "video_count")?,
            face_count: get(row, "face_count")?,
            person_count: get(row, "person_count")?,
            unknown_cluster_count: get(row, "unknown_cluster_count")?,
            pending_jobs: get(row, "pending_jobs")?,
            failed_jobs: get(row, "failed_jobs")?,
            processing_started_at: get(row, "processing_started_at")?,
            scan_completed_at: get(row, "scan_completed_at")?,
            processing_completed_at: get(row, "processing_completed_at")?,
            processing_duration_ms: get(row, "processing_duration_ms")?,
        })
    })
    .collect()
}

pub fn summary(conn: &mut dyn Db, id: i64) -> Result<Option<ShootSummary>> {
    Ok(list_summaries(conn)?.into_iter().find(|s| s.shoot.id == id))
}

pub fn set_status(conn: &mut dyn Db, id: i64, status: ShootStatus) -> Result<()> {
    conn.exec(
        "UPDATE shoots SET status = $2, updated_at = $3 WHERE id = $1",
        params![id, status.as_str(), now()],
    )?;
    Ok(())
}

pub fn rename(conn: &mut dyn Db, id: i64, name: &str) -> Result<()> {
    conn.exec(
        "UPDATE shoots SET name = $2, updated_at = $3 WHERE id = $1",
        params![id, name, now()],
    )?;
    Ok(())
}

/// Records (or clears) the path other machines use to reach this shoot's
/// folder. `source_path` is deliberately not touched: it is what the scanner
/// saw and what every `content_key` was derived from.
pub fn set_share_path(conn: &mut dyn Db, id: i64, share_path: Option<&str>) -> Result<()> {
    let share_path = share_path.map(str::trim).filter(|s| !s.is_empty());
    conn.exec(
        "UPDATE shoots SET share_path = $2, updated_at = $3 WHERE id = $1",
        params![id, share_path, now()],
    )?;
    Ok(())
}

pub fn set_notes(conn: &mut dyn Db, id: i64, notes: Option<&str>) -> Result<()> {
    conn.exec(
        "UPDATE shoots SET notes = $2, updated_at = $3 WHERE id = $1",
        params![id, notes, now()],
    )?;
    Ok(())
}

/// Removes the shoot's **index** only. Faces, albums and jobs cascade away;
/// the user's media on disk is untouched (§21).
pub fn delete_index(conn: &mut dyn Db, id: i64) -> Result<()> {
    conn.exec("DELETE FROM shoots WHERE id = $1", params![id])?;
    Ok(())
}

/// Removes only the requested shoot indexes, ignoring ids that no longer
/// exist. The caller supplies an explicit list so this can never widen into a
/// database-wide clear by accident.
pub fn delete_indexes(conn: &mut dyn Db, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    // One statement with `= ANY($1)` replaces the per-id loop the prepared
    // statement made cheap under SQLite; here each execute is a round trip.
    // Duplicate ids in the list collapse, which is what the old loop did too.
    let unique: Vec<i64> = {
        let mut v = ids.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };
    let removed = conn.exec("DELETE FROM shoots WHERE id = ANY($1)", params![unique])?;
    Ok(removed as usize)
}

/// Removes every scanned shoot index. Shoot-owned media, faces, clusters,
/// albums, jobs and exports cascade away; global settings and player profiles
/// are deliberately retained. Source media is never touched.
pub fn clear_all_indexes(conn: &mut dyn Db) -> Result<usize> {
    let removed = conn.exec("DELETE FROM shoots", params![])?;
    Ok(removed as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn create_and_summarise() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();

        let shoot = create(&mut conn, "BGMS Finals Player Shoot", "D:\\BGMS_Final_Shoot").unwrap();
        assert_eq!(shoot.status, "created");

        let summaries = list_summaries(&mut conn).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].photo_count, 0);
        assert_eq!(summaries[0].video_count, 0);

        set_status(&mut conn, shoot.id, ShootStatus::Completed).unwrap();
        assert_eq!(get_by_id(&mut conn, shoot.id).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn share_path_is_separate_from_the_scanned_source_path() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = create(&mut conn, "Finals", "D:\\shoots\\finals").unwrap();
        assert_eq!(shoot.share_path, None, "a new shoot is server-only until mapped");

        set_share_path(&mut conn, shoot.id, Some("\\\\STUDIO-PC\\shoots\\finals")).unwrap();
        let mapped = get_by_id(&mut conn, shoot.id).unwrap().unwrap();
        assert_eq!(mapped.share_path.as_deref(), Some("\\\\STUDIO-PC\\shoots\\finals"));
        assert_eq!(mapped.source_path, "D:\\shoots\\finals", "the scanner's path is untouched");

        set_share_path(&mut conn, shoot.id, Some("   ")).unwrap();
        assert_eq!(
            get_by_id(&mut conn, shoot.id).unwrap().unwrap().share_path,
            None,
            "blank means unmapped, not a share called ' '"
        );
    }

    #[test]
    fn reference_library_is_hidden_and_reused() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        create(&mut conn, "Real Shoot", "D:\\real").unwrap();

        let first = get_or_create_reference_library(&mut conn).unwrap();
        let second = get_or_create_reference_library(&mut conn).unwrap();
        assert_eq!(first.id, second.id, "the reference shoot is a singleton");

        assert_eq!(
            list(&mut conn).unwrap().len(),
            1,
            "the reference shoot never appears in normal listings"
        );
        assert_eq!(list_summaries(&mut conn).unwrap().len(), 1);
    }

    #[test]
    fn clearing_scanned_indexes_preserves_settings() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        create(&mut conn, "One", "D:\\one").unwrap();
        create(&mut conn, "Two", "D:\\two").unwrap();
        conn.exec("INSERT INTO settings (key, value) VALUES ('theme', 'dark')", params![])
            .unwrap();

        assert_eq!(clear_all_indexes(&mut conn).unwrap(), 2);
        assert!(list(&mut conn).unwrap().is_empty());
        let setting: String = conn
            .row_one("SELECT value FROM settings WHERE key = 'theme'", params![])
            .unwrap()
            .get(0);
        assert_eq!(setting, "dark");
    }

    #[test]
    fn clearing_selected_indexes_preserves_unselected_shoots() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let one = create(&mut conn, "One", "D:\\one").unwrap();
        let two = create(&mut conn, "Two", "D:\\two").unwrap();
        let three = create(&mut conn, "Three", "D:\\three").unwrap();

        assert_eq!(
            delete_indexes(&mut conn, &[one.id, three.id, three.id, 999_999]).unwrap(),
            2
        );
        let remaining = list(&mut conn).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, two.id);
    }

    /// The reference shoot is excluded from listings but must still be counted
    /// when the whole index is cleared, or it would survive a "clear all".
    #[test]
    fn clearing_all_indexes_includes_the_reference_shoot() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        create(&mut conn, "One", "D:\\one").unwrap();
        get_or_create_reference_library(&mut conn).unwrap();

        assert_eq!(clear_all_indexes(&mut conn).unwrap(), 2);
        assert!(reference_library_id(&mut conn).unwrap().is_none());
    }
}
