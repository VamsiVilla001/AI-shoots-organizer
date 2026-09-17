//! Logical record bytes, not an allocation of shared pages, WAL or indexes.
//!
//! The SQLite version summed `length(CAST(col AS BLOB))` per column, walking
//! `PRAGMA table_info` to find them, because SQLite is dynamically typed and
//! had no per-row size function. Postgres has `pg_column_size`, which is both
//! the right answer and one expression instead of a generated one — it reports
//! the stored size of a value including its varlena header, and for a whole row
//! (`pg_column_size(t.*)`) the tuple as the heap holds it.
//!
//! One difference worth knowing before reading a number here against an old
//! SQLite one: `pg_column_size` reports the size **as stored**, so a value
//! Postgres compressed (TOAST, for anything wide) counts compressed. Face
//! embeddings are packed `f32`s and compress barely at all, so in practice the
//! totals track. A column of highly repetitive data would read smaller here
//! than its logical length — which is the honest answer to "how much room is
//! this shoot taking up", the question the Settings screen is asking.

use crate::client::Db;
use crate::{params, Result};

pub fn shoot_record_bytes(conn: &mut dyn Db, shoot_id: i64) -> Result<u64> {
    let mut total: i64 = 0;
    // Identifiers and predicates are application constants, never user input.
    for (table, predicate) in [
        ("shoots", "id = $1"),
        ("media", "shoot_id = $1"),
        ("faces", "shoot_id = $1"),
        ("clusters", "shoot_id = $1"),
        ("albums", "shoot_id = $1"),
        ("media_groups", "shoot_id = $1"),
        ("jobs", "shoot_id = $1"),
        (
            "video_detections",
            "media_id IN (SELECT id FROM media WHERE shoot_id = $1)",
        ),
        (
            "video_sample_frames",
            "media_id IN (SELECT id FROM media WHERE shoot_id = $1)",
        ),
        ("album_media", "album_id IN (SELECT id FROM albums WHERE shoot_id = $1)"),
        (
            "media_group_items",
            "group_id IN (SELECT id FROM media_groups WHERE shoot_id = $1)",
        ),
    ] {
        let row = conn.row_one(
            &format!("SELECT COALESCE(SUM(pg_column_size(t.*)), 0)::bigint FROM {table} AS t WHERE {predicate}"),
            params![shoot_id],
        )?;
        total += super::at::<i64>(&row, 0)?;
    }
    Ok(total.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{repo::shoots, Database};

    /// Bytes that do not compress, so `pg_column_size` reports what was stored
    /// rather than what pglz managed to squeeze it down to. A xorshift keeps it
    /// deterministic without pulling in a dependency.
    fn incompressible(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    /// Exact byte totals are a property of the storage engine, so this asserts
    /// the relationships the Settings screen actually depends on rather than
    /// the numbers the SQLite version could pin down: scoped to one shoot,
    /// growing with content, and never destructive.
    #[test]
    fn counts_only_the_requested_shoot_and_grows_with_its_records() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();

        assert_eq!(shoot_record_bytes(&mut conn, 999).unwrap(), 0, "an unknown shoot is 0");

        let first = shoots::create(&mut conn, "First", "C:/first").unwrap();
        let baseline = shoot_record_bytes(&mut conn, first.id).unwrap();
        assert!(baseline > 0);

        // Another shoot's rows must not be counted against this one.
        shoots::create(&mut conn, "Other", "C:/other").unwrap();
        assert_eq!(shoot_record_bytes(&mut conn, first.id).unwrap(), baseline);

        // A longer name is at least as many bytes. Not *exactly* four more:
        // `pg_column_size` reports the stored tuple, and a short string's
        // varlena header and the row's alignment padding can absorb a few
        // characters. The panel shows an approximate size, so "grows, and never
        // shrinks" is the property worth pinning — an exact delta would only
        // pin this test to Postgres' tuple layout.
        conn.exec(
            "UPDATE shoots SET name = name || 'abcd' WHERE id = $1",
            params![first.id],
        )
        .unwrap();
        assert!(shoot_record_bytes(&mut conn, first.id).unwrap() >= baseline);

        let media_id: i64 = conn
            .row_one(
                "INSERT INTO media (shoot_id, path, filename, media_type, extension, content_key, indexed_at)
                 VALUES ($1, 'a.jpg', 'a.jpg', 'photo', 'jpg', 'key', 'now') RETURNING id",
                params![first.id],
            )
            .unwrap()
            .get(0);

        // Incompressible bytes, not a block of zeros: `pg_column_size` reports
        // the *stored* size, and 2 KiB of zeros TOAST-compresses away to
        // nothing. A real embedding is packed `f32`s with no such structure, so
        // this stands in for one.
        let embedding = incompressible(2048);
        conn.exec(
            "INSERT INTO faces (media_id, shoot_id, embedding, bbox_x, bbox_y, bbox_w, bbox_h, created_at, detection_confidence)
             VALUES ($1, $2, $3, 0, 0, 1, 1, 'now', 1)",
            params![media_id, first.id, embedding],
        )
        .unwrap();
        let with_vector = shoot_record_bytes(&mut conn, first.id).unwrap();
        assert!(with_vector > baseline + 2000, "a 2 KiB embedding shows up");

        // Doubling it roughly doubles the contribution.
        conn.exec(
            "UPDATE faces SET embedding = $2 WHERE media_id = $1",
            params![media_id, incompressible(4096)],
        )
        .unwrap();
        assert!(shoot_record_bytes(&mut conn, first.id).unwrap() > with_vector + 2000);

        let before_frame = shoot_record_bytes(&mut conn, first.id).unwrap();
        conn.exec(
            "INSERT INTO video_sample_frames (media_id, timestamp, created_at) VALUES ($1, 5, 'now')",
            params![media_id],
        )
        .unwrap();
        assert!(shoot_record_bytes(&mut conn, first.id).unwrap() > before_frame);

        // Measuring must never delete anything.
        assert!(shoots::get_by_id(&mut conn, first.id).unwrap().is_some());
    }
}
