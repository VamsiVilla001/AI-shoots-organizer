//! Machines enrolled as workers, and the roster built from them.

use postgres::Row;
use serde::{Deserialize, Serialize};

use super::get;
use crate::client::Db;
use crate::{now, params, Result};

/// What a worker advertises when it enrols and on every claim.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Capabilities {
    pub app_version: String,
    pub gpu: Option<String>,
    pub detector_hash: Option<String>,
    pub embedder_hash: Option<String>,
    pub ai_workers: usize,
    pub on_battery: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub enrolled_by: Option<String>,
    pub enrolled_at: String,
    pub last_seen: Option<String>,
    pub capabilities: Capabilities,
    pub revoked_at: Option<String>,
}

/// One row of the worker roster: a machine plus what it is doing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineRosterEntry {
    #[serde(flatten)]
    pub machine: Machine,
    pub running: i64,
    pub completed: i64,
    pub failed: i64,
}

fn map(row: &Row) -> Result<Machine> {
    let capabilities: Option<String> = get(row, "capabilities")?;
    Ok(Machine {
        id: get(row, "id")?,
        name: get(row, "name")?,
        enrolled_by: get(row, "enrolled_by")?,
        enrolled_at: get(row, "enrolled_at")?,
        last_seen: get(row, "last_seen")?,
        capabilities: capabilities
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default(),
        revoked_at: get(row, "revoked_at")?,
    })
}

/// Hashes a machine token the way the table stores it.
pub fn token_hash(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

pub fn enrol(
    conn: &mut dyn Db,
    id: &str,
    name: &str,
    token: &str,
    enrolled_by: Option<&str>,
    capabilities: &Capabilities,
) -> Result<Machine> {
    let row = conn.row_one(
        "INSERT INTO machines (id, name, token_hash, enrolled_by, enrolled_at, capabilities)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (id) DO UPDATE SET
             name = excluded.name, token_hash = excluded.token_hash,
             enrolled_by = excluded.enrolled_by, enrolled_at = excluded.enrolled_at,
             capabilities = excluded.capabilities, revoked_at = NULL
         RETURNING *",
        params![
            id,
            name,
            token_hash(token),
            enrolled_by,
            now(),
            serde_json::to_string(capabilities)?
        ],
    )?;
    map(&row)
}

/// The machine behind a token, if it is enrolled and not revoked.
pub fn authenticate(conn: &mut dyn Db, token: &str) -> Result<Option<Machine>> {
    conn.row_opt(
        "SELECT * FROM machines WHERE token_hash = $1 AND revoked_at IS NULL",
        params![token_hash(token)],
    )?
    .as_ref()
    .map(map)
    .transpose()
}

/// Records a sighting and whatever the machine now advertises.
pub fn touch(conn: &mut dyn Db, id: &str, capabilities: &Capabilities) -> Result<()> {
    conn.exec(
        "UPDATE machines SET last_seen = $2, capabilities = $3 WHERE id = $1",
        params![id, now(), serde_json::to_string(capabilities)?],
    )?;
    Ok(())
}

pub fn revoke(conn: &mut dyn Db, id: &str) -> Result<bool> {
    let n = conn.exec(
        "UPDATE machines SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL",
        params![id, now()],
    )?;
    Ok(n == 1)
}

pub fn get_by_id(conn: &mut dyn Db, id: &str) -> Result<Option<Machine>> {
    conn.row_opt("SELECT * FROM machines WHERE id = $1", params![id])?
        .as_ref()
        .map(map)
        .transpose()
}

/// Every enrolled machine with what it is running and has done. Completed
/// and failed counts come from the jobs still in the table, so they are a
/// recent-history figure rather than an all-time one.
pub fn roster(conn: &mut dyn Db) -> Result<Vec<MachineRosterEntry>> {
    conn.rows(
        "SELECT m.*,
                (SELECT COUNT(*) FROM jobs j WHERE j.owner = m.id AND j.state = 'running') AS running,
                (SELECT COUNT(*) FROM jobs j WHERE j.owner = m.id AND j.state = 'done')    AS completed,
                (SELECT COUNT(*) FROM jobs j WHERE j.owner = m.id AND j.state = 'failed')  AS failed
           FROM machines m
          ORDER BY m.revoked_at IS NOT NULL, m.name",
        params![],
    )?
    .iter()
    .map(|row| {
        Ok(MachineRosterEntry {
            machine: map(row)?,
            running: get(row, "running")?,
            completed: get(row, "completed")?,
            failed: get(row, "failed")?,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn enrolment_hands_out_a_token_that_authenticates_until_revoked() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let capabilities = Capabilities {
            app_version: "2.0.0".into(),
            gpu: Some("DirectML".into()),
            detector_hash: Some("d".into()),
            embedder_hash: Some("e".into()),
            ai_workers: 2,
            on_battery: false,
        };
        let machine = enrol(&mut conn, "laptop-1", "Editing laptop", "secret-token", Some("acct"), &capabilities).unwrap();
        assert_eq!(machine.capabilities, capabilities);
        assert_eq!(authenticate(&mut conn, "secret-token").unwrap().unwrap().id, "laptop-1");
        assert!(authenticate(&mut conn, "wrong").unwrap().is_none());

        touch(&mut conn, "laptop-1", &Capabilities { on_battery: true, ..capabilities.clone() }).unwrap();
        let seen = get_by_id(&mut conn, "laptop-1").unwrap().unwrap();
        assert!(seen.last_seen.is_some());
        assert!(seen.capabilities.on_battery);

        assert!(revoke(&mut conn, "laptop-1").unwrap());
        assert!(authenticate(&mut conn, "secret-token").unwrap().is_none(), "revoked at the next claim");
        assert!(!revoke(&mut conn, "laptop-1").unwrap(), "already revoked");
        assert_eq!(roster(&mut conn).unwrap().len(), 1);
    }
}
