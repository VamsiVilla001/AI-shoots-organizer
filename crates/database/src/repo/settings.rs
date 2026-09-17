//! Key/value application settings, stored as JSON so the shape can grow.

use serde::{de::DeserializeOwned, Serialize};

use crate::client::Db;
use crate::{params, Result};

pub fn get_raw(conn: &mut dyn Db, key: &str) -> Result<Option<String>> {
    conn.row_opt("SELECT value FROM settings WHERE key = $1", params![key])?
        .as_ref()
        .map(|row| super::at(row, 0))
        .transpose()
}

pub fn set_raw(conn: &mut dyn Db, key: &str, value: &str) -> Result<()> {
    conn.exec(
        "INSERT INTO settings (key, value) VALUES ($1, $2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Reads a setting, falling back to `default` when it is missing *or* stored in
/// a shape this build no longer understands.
pub fn get<T: DeserializeOwned>(conn: &mut dyn Db, key: &str, default: T) -> Result<T> {
    match get_raw(conn, key)? {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or(default)),
        None => Ok(default),
    }
}

pub fn set<T: Serialize>(conn: &mut dyn Db, key: &str, value: &T) -> Result<()> {
    set_raw(conn, key, &serde_json::to_string(value)?)
}

pub fn all(conn: &mut dyn Db) -> Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    for row in conn.rows("SELECT key, value FROM settings", params![])? {
        out.insert(super::at::<String>(&row, 0)?, super::at::<String>(&row, 1)?);
    }
    Ok(out)
}

pub fn delete(conn: &mut dyn Db, key: &str) -> Result<()> {
    conn.exec("DELETE FROM settings WHERE key = $1", params![key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn roundtrips_and_falls_back() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();

        assert_eq!(get::<f64>(&mut conn, "threshold", 0.42).unwrap(), 0.42);
        set(&mut conn, "threshold", &0.55_f64).unwrap();
        assert_eq!(get::<f64>(&mut conn, "threshold", 0.42).unwrap(), 0.55);

        // A value written by an older shape must not crash the read.
        set_raw(&mut conn, "threshold", "\"not a number\"").unwrap();
        assert_eq!(get::<f64>(&mut conn, "threshold", 0.42).unwrap(), 0.42);
    }
}
