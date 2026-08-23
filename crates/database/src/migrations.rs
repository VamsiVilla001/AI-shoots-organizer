//! Forward-only schema migrations, tracked with SQLite's `user_version`.
//!
//! To change the schema, append a new entry to [`MIGRATIONS`]. Never edit an
//! existing one — installed databases have already run it.

use rusqlite::Connection;

use crate::Result;

struct Migration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        sql: include_str!("schema.sql"),
    },
    Migration {
        version: 2,
        name: "manual_groups",
        sql: include_str!("schema_002_groups.sql"),
    },
];

/// The schema version this build expects.
pub fn target_version() -> i32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn current_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Applies every migration newer than the database's `user_version`.
pub fn run(conn: &mut Connection) -> Result<()> {
    let installed = current_version(conn)?;

    for migration in MIGRATIONS.iter().filter(|m| m.version > installed) {
        tracing::info!(version = migration.version, name = migration.name, "applying migration");
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn migrates_to_target_and_is_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let mut conn = db.conn().unwrap();
        assert_eq!(current_version(&conn).unwrap(), target_version());

        // Running again must be a no-op rather than an error.
        run(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }

    /// The upgrade path real installations take: a database created before
    /// manual grouping existed must gain the new tables without losing a row.
    #[test]
    fn a_version_one_database_upgrades_in_place() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO shoots (id, name, source_path, created_at, updated_at)
             VALUES (1, 'BGMS Finals', 'D:\\raw', 'now', 'now')",
            [],
        )
        .unwrap();

        run(&mut conn).unwrap();

        assert_eq!(current_version(&conn).unwrap(), target_version());
        let shoots: i64 = conn.query_row("SELECT COUNT(*) FROM shoots", [], |r| r.get(0)).unwrap();
        assert_eq!(shoots, 1, "existing rows survive the upgrade");
        let groups: i64 = conn
            .query_row("SELECT COUNT(*) FROM media_groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(groups, 0, "the new tables exist and start empty");
    }

    #[test]
    fn foreign_keys_cascade_from_shoots() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO shoots (id, name, source_path, created_at, updated_at) VALUES (1, 'a', 'p', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO media (id, shoot_id, path, filename, media_type, extension, content_key, indexed_at)
             VALUES (1, 1, 'p/a.jpg', 'a.jpg', 'photo', 'jpg', 'k', 'now')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM shoots WHERE id = 1", []).unwrap();

        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0, "media rows should cascade away with their shoot");
    }
}
