//! Key/value application settings, stored as JSON so the shape can grow.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

use crate::Result;

pub fn get_raw(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
        .optional()?)
}

pub fn set_raw(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Reads a setting, falling back to `default` when it is missing *or* stored in
/// a shape this build no longer understands.
pub fn get<T: DeserializeOwned>(conn: &Connection, key: &str, default: T) -> Result<T> {
    match get_raw(conn, key)? {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or(default)),
        None => Ok(default),
    }
}

pub fn set<T: Serialize>(conn: &Connection, key: &str, value: &T) -> Result<()> {
    set_raw(conn, key, &serde_json::to_string(value)?)
}

pub fn all(conn: &Connection) -> Result<std::collections::HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let mut out = std::collections::HashMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

pub fn delete(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn roundtrips_and_falls_back() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();

        assert_eq!(get::<f64>(&conn, "threshold", 0.42).unwrap(), 0.42);
        set(&conn, "threshold", &0.55_f64).unwrap();
        assert_eq!(get::<f64>(&conn, "threshold", 0.42).unwrap(), 0.55);

        // A value written by an older shape must not crash the read.
        set_raw(&conn, "threshold", "\"not a number\"").unwrap();
        assert_eq!(get::<f64>(&conn, "threshold", 0.42).unwrap(), 0.42);
    }
}
