//! Query helpers, grouped by table.
//!
//! Every function takes a `&Connection` rather than the pool so callers can
//! compose several of them inside one transaction.

pub mod albums;
pub mod clusters;
pub mod exports;
pub mod faces;
pub mod groups;
pub mod jobs;
pub mod logs;
pub mod media;
pub mod people;
pub mod settings;
pub mod shoots;
pub mod video;

use rusqlite::Row;

/// `row.get(name)` with the column-name lookup already unwrapped into our error
/// type, so call sites stay readable.
pub(crate) fn get<T: rusqlite::types::FromSql>(row: &Row<'_>, name: &str) -> rusqlite::Result<T> {
    row.get(name)
}
