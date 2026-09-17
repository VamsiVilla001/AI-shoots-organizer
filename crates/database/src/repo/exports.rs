use postgres::Row;

use super::get;
use crate::client::Db;
use crate::models::{ExportRecord, ExportStatus};
use crate::{now, params, Result};

fn map(row: &Row) -> Result<ExportRecord> {
    Ok(ExportRecord {
        id: get(row, "id")?,
        shoot_id: get(row, "shoot_id")?,
        destination: get(row, "destination")?,
        options: get(row, "options")?,
        status: get(row, "status")?,
        files_total: get(row, "files_total")?,
        files_done: get(row, "files_done")?,
        bytes_done: get(row, "bytes_done")?,
        error: get(row, "error")?,
        started_at: get(row, "started_at")?,
        finished_at: get(row, "finished_at")?,
    })
}

pub fn create(conn: &mut dyn Db, shoot_id: i64, destination: &str, options_json: &str) -> Result<i64> {
    // `RETURNING id` replaces `last_insert_rowid()`, which had no Postgres
    // equivalent that is safe under a connection pool.
    let row = conn.row_one(
        "INSERT INTO exports (shoot_id, destination, options, status, started_at)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
        params![
            shoot_id,
            destination,
            options_json,
            ExportStatus::Running.as_str(),
            now()
        ],
    )?;
    get(&row, "id")
}

pub fn set_total(conn: &mut dyn Db, id: i64, files_total: i64) -> Result<()> {
    conn.exec(
        "UPDATE exports SET files_total = $2 WHERE id = $1",
        params![id, files_total],
    )?;
    Ok(())
}

pub fn set_progress(conn: &mut dyn Db, id: i64, files_done: i64, bytes_done: i64) -> Result<()> {
    conn.exec(
        "UPDATE exports SET files_done = $2, bytes_done = $3 WHERE id = $1",
        params![id, files_done, bytes_done],
    )?;
    Ok(())
}

pub fn finish(conn: &mut dyn Db, id: i64, status: ExportStatus, error: Option<&str>) -> Result<()> {
    conn.exec(
        "UPDATE exports SET status = $2, error = $3, finished_at = $4 WHERE id = $1",
        params![id, status.as_str(), error, now()],
    )?;
    Ok(())
}

pub fn get_by_id(conn: &mut dyn Db, id: i64) -> Result<Option<ExportRecord>> {
    conn.row_opt("SELECT * FROM exports WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

pub fn list(conn: &mut dyn Db, shoot_id: i64, limit: i64) -> Result<Vec<ExportRecord>> {
    conn.rows(
        "SELECT * FROM exports WHERE shoot_id = $1 ORDER BY id DESC LIMIT $2",
        params![shoot_id, limit.clamp(1, 200)],
    )?
    .iter()
    .map(map)
    .collect()
}
