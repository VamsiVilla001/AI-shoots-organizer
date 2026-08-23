//! Lightweight application log (§25).
//!
//! Records *what happened to which file and player* — imports, corrections,
//! renames, merges, exports. It deliberately never stores embeddings, crops or
//! any other biometric payload; the identifiers are enough to audit a decision.

use rusqlite::{params, Connection, Row};

use super::get;
use crate::models::LogEntry;
use crate::{now, Result};

pub const EVENT_SHOOT_IMPORTED: &str = "shoot_imported";
pub const EVENT_SHOOT_DELETED: &str = "shoot_index_deleted";
pub const EVENT_PROCESSING_ERROR: &str = "processing_error";
pub const EVENT_PLAYER_CREATED: &str = "player_created";
pub const EVENT_PLAYER_RENAMED: &str = "player_renamed";
pub const EVENT_PLAYER_MERGED: &str = "player_merged";
pub const EVENT_PLAYER_DELETED: &str = "player_deleted";
pub const EVENT_CLUSTER_NAMED: &str = "cluster_named";
pub const EVENT_CLUSTER_MERGED: &str = "cluster_merged";
pub const EVENT_CLUSTER_SPLIT: &str = "cluster_split";
pub const EVENT_PLAYER_ASSIGNMENT: &str = "player_assignment";
pub const EVENT_MANUAL_CORRECTION: &str = "manual_correction";
pub const EVENT_GROUP_CREATED: &str = "group_created";
pub const EVENT_GROUP_RENAMED: &str = "group_renamed";
pub const EVENT_GROUP_DELETED: &str = "group_deleted";
pub const EVENT_GROUP_ASSIGNMENT: &str = "group_assignment";
pub const EVENT_EXPORT: &str = "export";
pub const EVENT_RECOGNITION_DATA_CLEARED: &str = "recognition_data_cleared";

fn map(row: &Row<'_>) -> rusqlite::Result<LogEntry> {
    Ok(LogEntry {
        id: get(row, "id")?,
        timestamp: get(row, "timestamp")?,
        event: get(row, "event")?,
        shoot_id: get(row, "shoot_id")?,
        media_id: get(row, "media_id")?,
        person_id: get(row, "person_id")?,
        detail: get(row, "detail")?,
    })
}

pub fn record(
    conn: &Connection,
    event: &str,
    shoot_id: Option<i64>,
    media_id: Option<i64>,
    person_id: Option<i64>,
    detail: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO app_log (timestamp, event, shoot_id, media_id, person_id, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now(), event, shoot_id, media_id, person_id, detail],
    )?;
    Ok(())
}

/// Logging must never take down the operation it is describing.
pub fn record_quiet(
    conn: &Connection,
    event: &str,
    shoot_id: Option<i64>,
    media_id: Option<i64>,
    person_id: Option<i64>,
    detail: Option<&str>,
) {
    if let Err(e) = record(conn, event, shoot_id, media_id, person_id, detail) {
        tracing::warn!(event, error = %e, "failed to write app log entry");
    }
}

pub fn recent(conn: &Connection, shoot_id: Option<i64>, limit: i64) -> Result<Vec<LogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM app_log WHERE (?1 IS NULL OR shoot_id = ?1) ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![shoot_id, limit.clamp(1, 2_000)], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Keeps the log from growing without bound; called after each import.
pub fn trim(conn: &Connection, keep: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM app_log WHERE id NOT IN (SELECT id FROM app_log ORDER BY id DESC LIMIT ?1)",
        params![keep.max(100)],
    )?)
}

pub fn clear(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_log", [])?;
    Ok(())
}
