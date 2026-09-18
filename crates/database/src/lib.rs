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
pub mod paths;
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

    // --- why a connection failed -------------------------------------------
    //
    // These three are the difference between "your server is down", "this
    // machine has no password" and "that password is wrong" — problems with
    // nothing in common, which the pool reports almost identically. Everything
    // goes through r2d2, whose `Error` flattens the cause to one of three
    // strings and keeps no SQLSTATE:
    //
    //   nothing listening ............ "error connecting to server"
    //   password required, not given.. "invalid configuration"
    //   server answered and refused .. "db error"
    //
    // Matching text is unpleasant and driver-Display-dependent, so
    // `config::tests::connection_failures_are_classified_by_cause` pins all
    // three against a real server: a reworded driver fails the test rather
    // than silently sending people to check a firewall that is fine.

    /// A server answered, but no password was supplied and it wanted one.
    ///
    /// `password` is the only field [`PgConfig::to_postgres_config`] leaves
    /// unset, so there is nothing else "invalid configuration" can mean here.
    pub fn is_missing_credential(&self) -> bool {
        self.to_string().contains("invalid configuration")
    }

    /// A server answered and rejected us: wrong password, unknown user, no such
    /// database, no permission.
    pub fn is_rejected(&self) -> bool {
        match self {
            // Reached directly rather than through the pool: a rejected
            // statement always carries a SQLSTATE.
            DbError::Postgres(e) => e.code().is_some(),
            DbError::Pool(_) => self.to_string().contains("db error"),
            _ => false,
        }
    }

    /// Nothing answered — the server is down, unreachable, or firewalled.
    ///
    /// This is the case the UI turns into "start the server" or "check the
    /// network", so the other two have to be ruled out first: both also arrive
    /// without a SQLSTATE and would otherwise be mistaken for it.
    pub fn is_unavailable(&self) -> bool {
        if self.is_missing_credential() || self.is_rejected() {
            return false;
        }
        match self {
            DbError::Pool(_) => true,
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

    /// How long a test schema is kept before the next run sweeps it.
    ///
    /// Long enough to still be there when you go and look at why a test failed,
    /// short enough that no *live* test process could still own one — a suite
    /// that ran for an hour would have other problems.
    const SCHEMA_RETENTION: &str = "1 hour";

    /// Drops schemas left by runs that have long since finished.
    ///
    /// This is the half that was missing. Each schema is named for the process
    /// that made it, and a PID never repeats within a run, so nothing ever
    /// collided and nothing was ever reclaimed: 1795 schemas and 2.6 GB after a
    /// few days of `cargo test`. Sweeping on the way *in* rather than dropping
    /// on the way *out* is deliberate — a panicking test poisons its pool and
    /// would skip a cleanup-on-drop anyway, and dropping immediately would
    /// destroy exactly the rows you want to inspect after a failure.
    fn sweep_stale_schemas(client: &mut postgres::Client) -> Result<()> {
        client.batch_execute(
            "CREATE TABLE IF NOT EXISTS public.test_schemas (
                 name       TEXT PRIMARY KEY,
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now()
             )",
        )?;

        let stale: Vec<String> = client
            .query(
                &format!(
                    "SELECT name FROM public.test_schemas WHERE created_at < now() - interval '{SCHEMA_RETENTION}'"
                ),
                &[],
            )?
            .iter()
            .map(|row| row.get(0))
            .collect();

        for name in stale {
            // One statement apiece, each its own transaction. Dropping them
            // together holds a lock on every object until commit, and ~30
            // tables per schema exhausts `max_locks_per_transaction` — the
            // error is "out of shared memory", which reads like a server
            // problem rather than a batching one.
            client.batch_execute(&format!("DROP SCHEMA IF EXISTS {name} CASCADE"))?;
            client.execute("DELETE FROM public.test_schemas WHERE name = $1", &[&name])?;
        }
        Ok(())
    }

    impl Database {
        /// A private, empty, migrated schema on the test server.
        ///
        /// Schemas are registered in `public.test_schemas` and swept by a later
        /// run once they are older than [`SCHEMA_RETENTION`], rather than
        /// dropped when the `Database` goes out of scope — see
        /// [`sweep_stale_schemas`] for why.
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
            // Once per process, not once per test. `cargo test` opens one of
            // these per test in parallel; without the guard a hundred threads
            // would each read the same stale list and race to drop the same
            // schemas — idempotent, but every one of them waits for it.
            static SWEEP: std::sync::Once = std::sync::Once::new();
            let mut swept = Ok(());
            SWEEP.call_once(|| swept = sweep_stale_schemas(&mut bootstrap));
            swept?;
            bootstrap.batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"
            ))?;
            // Registered before use, so a process that dies mid-test still
            // leaves a row for the next run to sweep.
            bootstrap.execute(
                "INSERT INTO public.test_schemas (name) VALUES ($1)
                 ON CONFLICT (name) DO UPDATE SET created_at = now()",
                &[&schema],
            )?;
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
