//! SQLite storage for the SKWAD Media Organiser.
//!
//! Everything the application knows lives here: the shoot index, detected faces
//! and their embeddings, the reusable player library, generated albums and the
//! resumable job queue. The database is an index over the user's media — it
//! never owns or mutates the source files.

pub mod migrations;
pub mod models;
pub mod repo;
mod vector;

use std::path::{Path, PathBuf};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

pub use rusqlite;
pub use vector::{blob_to_vec, vec_to_blob};

pub type DbPool = Pool<SqliteConnectionManager>;
pub type DbConn = r2d2::PooledConnection<SqliteConnectionManager>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Pool(#[from] r2d2::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DbError>;

impl DbError {
    pub fn other(msg: impl Into<String>) -> Self {
        DbError::Other(msg.into())
    }
}

/// A connection pool onto one `media.db`, with migrations already applied.
#[derive(Clone)]
pub struct Database {
    pool: DbPool,
    path: PathBuf,
}

impl Database {
    /// Opens (creating if needed) the database at `path` and brings the schema
    /// up to the current version.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DbError::other(format!("create {}: {e}", parent.display())))?;
        }

        let manager = SqliteConnectionManager::file(&path).with_init(|conn| {
            // WAL lets the UI read while background workers write.
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.pragma_update(None, "busy_timeout", 10_000)?;
            conn.pragma_update(None, "temp_store", "MEMORY")?;
            Ok(())
        });

        let pool = Pool::builder().max_size(8).build(manager)?;
        let mut conn = pool.get()?;
        migrations::run(&mut conn)?;

        Ok(Self { pool, path })
    }

    /// An in-memory database, used by tests.
    pub fn open_in_memory() -> Result<Self> {
        let manager = SqliteConnectionManager::memory().with_init(|conn| {
            conn.pragma_update(None, "foreign_keys", "ON")?;
            Ok(())
        });
        // A single connection, otherwise each pooled connection gets its own
        // private in-memory database.
        let pool = Pool::builder().max_size(1).build(manager)?;
        let mut conn = pool.get()?;
        migrations::run(&mut conn)?;
        drop(conn);
        Ok(Self { pool, path: PathBuf::from(":memory:") })
    }

    pub fn conn(&self) -> Result<DbConn> {
        Ok(self.pool.get()?)
    }

    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` inside a transaction, committing on `Ok` and rolling back on `Err`.
    pub fn transaction<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Reclaims space after a large delete. Cheap enough to call on demand,
    /// too expensive to call automatically.
    pub fn vacuum(&self) -> Result<()> {
        self.conn()?.execute_batch("VACUUM")?;
        Ok(())
    }
}

/// The current UTC timestamp in the format every `*_at` column uses.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
