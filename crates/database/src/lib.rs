//! PostgreSQL storage for the SKWAD Media Organiser.
//!
//! Everything the application knows lives here: the shoot index, detected faces
//! and their embeddings, the reusable player library, generated albums and the
//! resumable job queue. The database is an index over the user's media — it
//! never owns or mutates the source files.
//!
//! ## Why a server, and what it cost
//!
//! Through v2.0.0-alpha.3 this was an embedded SQLite file under the library
//! folder. Postgres replaces it, which buys real concurrent writers (the worker
//! pool no longer serialises behind one writer lock) and makes a shared library
//! a server other machines connect to rather than a `.db` on an SMB share with
//! its journal mode downgraded. The cost is that the app is no longer
//! zero-install: `skwad-postgres` must be running before the app can open a
//! library. [`Database::connect`] fails loudly rather than falling back.
//!
//! An existing SQLite library is carried across by the `skwad-db-migrate`
//! binary in this crate — see `bin/migrate.rs`.

pub mod client;
pub mod config;
pub mod migrations;
pub mod models;
pub mod repo;
mod vector;

use postgres::NoTls;
use r2d2_postgres::PostgresConnectionManager;

pub use client::Db;
pub use config::PgConfig;
pub use postgres;
pub use vector::{blob_to_vec, vec_to_blob};

pub type DbPool = r2d2::Pool<PostgresConnectionManager<NoTls>>;
pub type DbConn = r2d2::PooledConnection<PostgresConnectionManager<NoTls>>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Postgres(#[from] postgres::Error),
    #[error(transparent)]
    Pool(#[from] r2d2::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A query that must match a row matched none. Under rusqlite this was
    /// `QueryReturnedNoRows`; callers that used `.optional()` now use
    /// [`Db::row_opt`] instead and never see this.
    #[error("no row for: {0}…")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DbError>;

impl DbError {
    pub fn other(msg: impl Into<String>) -> Self {
        DbError::Other(msg.into())
    }

    /// True when the failure was the server being unreachable rather than the
    /// statement being wrong — the case the UI turns into "start the local
    /// server" instead of a generic error.
    pub fn is_unavailable(&self) -> bool {
        match self {
            DbError::Pool(_) => true,
            // A connection-level failure carries no SQLSTATE; a rejected
            // statement always does.
            DbError::Postgres(e) => e.code().is_none(),
            _ => false,
        }
    }
}

/// A connection pool onto one SKWAD database, with migrations already applied.
#[derive(Clone)]
pub struct Database {
    pool: DbPool,
    config: PgConfig,
}

impl Database {
    /// Connects to the configured server, creating the schema if this is a new
    /// database, and brings it up to the current version.
    ///
    /// Unlike the SQLite implementation this cannot create the *database* — a
    /// server-side object needs rights the app should not hold. `skwad-postgres`
    /// provisioning does that once; see `docs/deployment.md`.
    pub fn connect(config: PgConfig) -> Result<Self> {
        let manager = PostgresConnectionManager::new(config.to_postgres_config(), NoTls);

        let pool = r2d2::Pool::builder()
            // Matches the old SQLite pool, and comfortably exceeds
            // "1 I/O worker + N AI workers + the UI thread".
            .max_size(config.max_connections)
            // `min_idle` defaults to `max_size`, which makes `build()` open
            // every connection up front and block until they all succeed. An
            // embedded SQLite pool could do that for free; against a server it
            // means the app holds eight sessions from launch whether or not it
            // is doing anything, and a slow server delays startup. Idle
            // connections are opened on demand instead.
            .min_idle(Some(0))
            .connection_timeout(config.connect_timeout())
            .build(manager)?;

        let mut conn = pool.get()?;
        migrations::run(&mut *conn)?;
        drop(conn);

        Ok(Self { pool, config })
    }

    pub fn conn(&self) -> Result<DbConn> {
        Ok(self.pool.get()?)
    }

    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    pub fn config(&self) -> &PgConfig {
        &self.config
    }

    /// How the library identifies itself in the UI, where the SQLite build
    /// showed a file path.
    pub fn describe(&self) -> String {
        self.config.describe()
    }

    /// Runs `f` inside a transaction, committing on `Ok` and rolling back on `Err`.
    pub fn transaction<T>(&self, f: impl FnOnce(&mut dyn Db) -> Result<T>) -> Result<T> {
        let mut conn = self.conn()?;
        let mut tx = conn.transaction()?;
        let out = f(&mut tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Reclaims space after a large delete. Cheap enough to call on demand,
    /// too expensive to call automatically.
    ///
    /// `VACUUM` cannot run inside a transaction block, so this deliberately
    /// takes a bare connection rather than going through [`Self::transaction`].
    pub fn vacuum(&self) -> Result<()> {
        self.conn()?.batch("VACUUM (ANALYZE)")
    }
}

/// The current UTC timestamp in the format every `*_at` column uses.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------- tests

/// Test support: a throwaway database, isolated per test.
///
/// The SQLite build used `:memory:`. Postgres has no equivalent, so each call
/// creates a uniquely-named schema on the test server and pins every pooled
/// connection's `search_path` to it — which gives the same property that
/// mattered: two tests running concurrently cannot see each other's rows.
///
/// Requires a reachable server. `SKWAD_TEST_DATABASE_URL` overrides the default
/// of `postgres://postgres:postgres@localhost:5432/skwad_test`; see
/// `docs/development.md` for creating it.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Pins each pooled connection to the test's private schema.
    #[derive(Debug)]
    struct SearchPath(String);

    impl r2d2::CustomizeConnection<postgres::Client, postgres::Error> for SearchPath {
        fn on_acquire(&self, conn: &mut postgres::Client) -> std::result::Result<(), postgres::Error> {
            conn.batch_execute(&format!("SET search_path TO {}", self.0))
        }
    }

    impl Database {
        /// A private, empty, migrated schema on the test server.
        ///
        /// Dropped schemas are cleaned up by the *next* run rather than on
        /// drop: a test that panics would otherwise leave the pool poisoned
        /// and the schema behind anyway, and a `DROP SCHEMA` on the way out
        /// makes a failing test's rows impossible to inspect.
        pub fn open_test() -> Result<Self> {
            let url = std::env::var("SKWAD_TEST_DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/skwad_test".to_string());
            let config = PgConfig::from_url(&url)
                .unwrap_or_else(|e| panic!("SKWAD_TEST_DATABASE_URL is not a valid connection string: {e}"));

            let schema = format!(
                "skwad_test_{}_{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );

            let mut bootstrap = postgres::Client::connect(&url, NoTls).unwrap_or_else(|e| {
                panic!(
                    "these tests need a PostgreSQL server at {url}: {e}\n\
                     Run `npm run db:setup` (or see docs/development.md) to create one."
                )
            });
            bootstrap.batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"
            ))?;
            drop(bootstrap);

            let manager = PostgresConnectionManager::new(config.to_postgres_config(), NoTls);
            let pool = r2d2::Pool::builder()
                // Two, not more: `cargo test` runs one of these pools per test
                // *concurrently*, so the ceiling is really
                // `max_size × test threads` against the server's
                // `max_connections`. On a 36-core machine a pool of four
                // exhausted the server outright. Two is the minimum that works,
                // because a test may hold a connection and still call
                // `Database::transaction`, which takes a second one.
                .max_size(2)
                // And opened only when actually asked for — see the note in
                // `connect`. Most tests use exactly one.
                .min_idle(Some(0))
                .connection_customizer(Box::new(SearchPath(schema)))
                .build(manager)?;

            let mut conn = pool.get()?;
            migrations::run(&mut *conn)?;
            drop(conn);

            Ok(Self { pool, config })
        }
    }
}
