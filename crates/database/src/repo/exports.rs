use rusqlite::{params, Connection, OptionalExtension, Row};

use super::get;
use crate::models::{ExportRecord, ExportStatus};
use crate::{now, Result};

fn map(row: &Row<'_>) -> rusqlite::Result<ExportRecord> {
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

pub fn create(conn: &Connection, shoot_id: i64, destination: &str, options_json: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO exports (shoot_id, destination, options, status, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![shoot_id, destination, options_json, ExportStatus::Running, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn set_total(conn: &Connection, id: i64, files_total: i64) -> Result<()> {
    conn.execute("UPDATE exports SET files_total = ?2 WHERE id = ?1", params![id, files_total])?;
    Ok(())
}

pub fn set_progress(conn: &Connection, id: i64, files_done: i64, bytes_done: i64) -> Result<()> {
    conn.execute(
        "UPDATE exports SET files_done = ?2, bytes_done = ?3 WHERE id = ?1",
        params![id, files_done, bytes_done],
    )?;
    Ok(())
}

pub fn finish(conn: &Connection, id: i64, status: ExportStatus, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE exports SET status = ?2, error = ?3, finished_at = ?4 WHERE id = ?1",
        params![id, status, error, now()],
    )?;
    Ok(())
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<ExportRecord>> {
    Ok(conn
        .prepare("SELECT * FROM exports WHERE id = ?1")?
        .query_row(params![id], map)
        .optional()?)
}

pub fn list(conn: &Connection, shoot_id: i64, limit: i64) -> Result<Vec<ExportRecord>> {
    let mut stmt =
        conn.prepare("SELECT * FROM exports WHERE shoot_id = ?1 ORDER BY id DESC LIMIT ?2")?;
    let rows = stmt
        .query_map(params![shoot_id, limit.clamp(1, 200)], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
