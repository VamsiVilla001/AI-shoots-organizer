//! Query helpers, grouped by table.
//!
//! Every function takes a `&mut dyn Db` rather than the pool, so callers can
//! compose several of them inside one transaction. Under rusqlite this was a
//! shared `&Connection`; Postgres needs `&mut` to run a statement, and `dyn Db`
//! (see [`crate::client`]) is what lets the same function accept either a
//! pooled connection or a transaction borrowed from one.

pub mod albums;
pub mod clusters;
pub mod exports;
pub mod faces;
pub mod groups;
pub mod jobs;
pub mod logs;
pub mod media;
pub mod people;
pub mod projects;
pub mod roster;
pub mod settings;
pub mod shoots;
pub mod storage;
pub mod telemetry;
pub mod video;

use postgres::types::FromSql;
use postgres::Row;

/// `row.get(name)` that returns an error instead of panicking on a missing or
/// mistyped column, so call sites stay readable and a schema drift shows up as
/// a `DbError` rather than a panic inside a worker thread.
pub(crate) fn get<'a, T: FromSql<'a>>(row: &'a Row, name: &str) -> crate::Result<T> {
    row.try_get(name)
        .map_err(|e| crate::DbError::other(format!("column `{name}`: {e}")))
}

/// `get`, for a column addressed by position — the shape aggregate queries use.
pub(crate) fn at<'a, T: FromSql<'a>>(row: &'a Row, index: usize) -> crate::Result<T> {
    row.try_get(index)
        .map_err(|e| crate::DbError::other(format!("column {index}: {e}")))
}
