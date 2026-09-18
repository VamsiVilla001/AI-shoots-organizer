//! Forward-only schema migrations, tracked in a `schema_migrations` table.
//!
//! To change the schema, append a new entry to [`MIGRATIONS`]. Never edit an
//! existing one — installed databases have already run it.
//!
//! The SQLite build tracked this in `PRAGMA user_version`, a single integer.
//! Postgres has no equivalent, and a table is better anyway: it records *when*
//! each migration ran, which is the first thing you want when a library behaves
//! differently from the one next to it.
//!
//! Versions 1..13 were the SQLite history. They are collapsed into a single
//! Postgres baseline, because no Postgres database has ever run them — an
//! existing SQLite library arrives through `skwad-db-migrate`, which copies
//! rows into the finished schema rather than replaying thirteen migrations.

use crate::client::Db;
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
        sql: include_str!("sql/001_baseline.sql"),
    },
    Migration {
        version: 2,
        name: "path_portability",
        sql: include_str!("sql/002_path_portability.sql"),
    },
    Migration {
        version: 3,
        name: "job_leases",
        sql: include_str!("sql/003_job_leases.sql"),
    },
    Migration {
        version: 4,
        name: "model_identity",
        sql: include_str!("sql/004_model_identity.sql"),
    },
];

/// The schema version this build expects.
pub fn target_version() -> i32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn current_version(conn: &mut dyn Db) -> Result<i32> {
    ensure_ledger(conn)?;
    let row = conn.row_one(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        crate::params![],
    )?;
    Ok(row.get(0))
}

/// Creates the bookkeeping table. Separate from the migrations themselves so
/// that reading the version from an empty database is not itself a migration.
fn ensure_ledger(conn: &mut dyn Db) -> Result<()> {
    conn.batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT        NOT NULL,
             applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
         )",
    )
}

/// Applies every migration this database has not already run.
///
/// Each runs in its own transaction, so a failure leaves the database at the
/// last version that fully applied rather than half-way through one.
pub fn run(conn: &mut dyn Db) -> Result<()> {
    ensure_ledger(conn)?;
    let installed = current_version(conn)?;

    for migration in MIGRATIONS.iter().filter(|m| m.version > installed) {
        tracing::info!(version = migration.version, name = migration.name, "applying migration");
        conn.batch("BEGIN")?;
        let applied = (|| -> Result<()> {
            conn.batch(migration.sql)?;
            conn.exec(
                "INSERT INTO schema_migrations (version, name) VALUES ($1, $2)",
                crate::params![migration.version, migration.name],
            )?;
            Ok(())
        })();

        match applied {
            Ok(()) => conn.batch("COMMIT")?,
            Err(error) => {
                conn.batch("ROLLBACK")?;
                return Err(error);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{params, Database};

    #[test]
    fn migrates_to_target_and_is_idempotent() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        assert_eq!(current_version(&mut conn).unwrap(), target_version());

        // Running again must be a no-op rather than an error.
        run(&mut conn).unwrap();
        assert_eq!(current_version(&mut conn).unwrap(), target_version());
    }

    #[test]
    fn foreign_keys_cascade_from_shoots() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        conn.exec(
            "INSERT INTO shoots (id, name, source_path, created_at, updated_at) VALUES (1, 'a', 'p', 'now', 'now')",
            params![],
        )
        .unwrap();
        conn.exec(
            "INSERT INTO media (id, shoot_id, path, filename, media_type, extension, content_key, indexed_at)
             VALUES (1, 1, 'p/a.jpg', 'a.jpg', 'photo', 'jpg', 'k', 'now')",
            params![],
        )
        .unwrap();
        conn.exec("DELETE FROM shoots WHERE id = 1", params![]).unwrap();

        let remaining: i64 = conn.row_one("SELECT COUNT(*) FROM media", params![]).unwrap().get(0);
        assert_eq!(remaining, 0, "media rows should cascade away with their shoot");
    }

    /// Foreign keys were enforced by a per-connection `PRAGMA foreign_keys = ON`
    /// under SQLite, which was easy to lose. In Postgres they are structural —
    /// this pins that a violation is actually rejected.
    #[test]
    fn a_dangling_foreign_key_is_refused() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let result = conn.exec(
            "INSERT INTO media (shoot_id, path, filename, media_type, extension, content_key, indexed_at)
             VALUES (999, 'p/a.jpg', 'a.jpg', 'photo', 'jpg', 'k', 'now')",
            params![],
        );
        assert!(result.is_err(), "media must not outlive its shoot");
    }

    /// The `nocase` collation stands in for SQLite's `COLLATE NOCASE`, and the
    /// unique index on `people.name` depends on it being non-deterministic.
    #[test]
    fn person_names_are_compared_case_insensitively() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        conn.exec(
            "INSERT INTO people (name, created_at, updated_at) VALUES ('Naresh', 'now', 'now')",
            params![],
        )
        .unwrap();

        let found = conn
            .row_opt("SELECT id FROM people WHERE name = $1", params!["NARESH"])
            .unwrap();
        assert!(found.is_some(), "lookup must ignore case");

        let duplicate = conn.exec(
            "INSERT INTO people (name, created_at, updated_at) VALUES ('naresh', 'now', 'now')",
            params![],
        );
        assert!(duplicate.is_err(), "the unique index must ignore case too");
    }

    /// The six `*_assign_stable_id` triggers became column defaults; this is
    /// the behaviour they existed to provide.
    #[test]
    fn inserted_rows_get_a_stable_id_without_being_asked() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let row = conn
            .row_one(
                "INSERT INTO shoots (name, source_path, created_at, updated_at)
                 VALUES ('BGMS Finals', 'D:\\raw', 'now', 'now')
                 RETURNING stable_id, library_id",
                params![],
            )
            .unwrap();
        let stable: String = row.get(0);
        let library: String = row.get(1);
        assert_eq!(stable.len(), 36, "a uuid, not an empty string");
        assert_eq!(library.len(), 36);
        assert_ne!(stable, library);
    }
}
