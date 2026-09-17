//! Team rosters: who plays for whom at an event.
//!
//! The roster is consulted while a reviewer names a face. Whatever they type is
//! looked up here; a hit tells SKWAD which team the person belongs to, which is
//! what lets naming one player file their media under the right team.
//!
//! Matching ignores case, spaces and punctuation, because the same player is
//! written "S8UL Naresh", "s8ul_naresh" and "iQOOS8ULNaresh" depending on who
//! typed it.

use postgres::Row;
use serde::{Deserialize, Serialize};

use super::get;
use crate::client::Db;
use crate::{params, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RosterEntry {
    #[serde(default)]
    pub id: i64,
    pub ign: String,
    #[serde(default)]
    pub player_name: String,
    pub team: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub source: String,
}

fn default_role() -> String {
    "player".to_owned()
}

/// Letters and digits only, lowercased: the form every comparison happens in.
pub fn normalise(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn search_key(entry: &RosterEntry) -> String {
    format!(
        "{} {} {}",
        normalise(&entry.ign),
        normalise(&entry.player_name),
        normalise(&entry.team)
    )
}

fn map(row: &Row) -> Result<RosterEntry> {
    Ok(RosterEntry {
        id: get(row, "id")?,
        ign: get(row, "ign")?,
        player_name: get(row, "player_name")?,
        team: get(row, "team")?,
        role: get(row, "role")?,
        source: get(row, "source")?,
    })
}

/// Replaces every row that came from `source` with `entries`.
///
/// Re-importing a corrected file therefore removes rows that were dropped from
/// it, while rosters imported from other files are left alone. An IGN that has
/// moved to another team simply updates.
pub fn replace_source(conn: &mut dyn Db, source: &str, entries: &[RosterEntry]) -> Result<usize> {
    conn.exec("DELETE FROM roster_entries WHERE source = $1", params![source])?;
    let now = crate::now();
    let mut written = 0usize;
    for entry in entries {
        // An IGN already claimed by another file is updated in place rather
        // than failing the whole import: the newest file wins.
        //
        // Deliberately still a row at a time rather than one multi-row insert:
        // a file that lists the same IGN twice is not rare, and Postgres
        // refuses an `ON CONFLICT DO UPDATE` that would touch the same row
        // twice within one statement. Row by row, the second line simply
        // updates the first — which is the behaviour this has always had.
        //
        // The conflict target is plain `(ign)`: the case-insensitivity that
        // `COLLATE NOCASE` used to spell out here now lives on the column.
        conn.exec(
            "INSERT INTO roster_entries (ign, player_name, team, role, search_key, source, imported_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (ign) DO UPDATE SET
                 player_name = excluded.player_name,
                 team = excluded.team,
                 role = excluded.role,
                 search_key = excluded.search_key,
                 source = excluded.source,
                 imported_at = excluded.imported_at",
            params![
                entry.ign,
                entry.player_name,
                entry.team,
                entry.role,
                search_key(entry),
                source,
                now
            ],
        )?;
        written += 1;
    }
    Ok(written)
}

pub fn list(conn: &mut dyn Db) -> Result<Vec<RosterEntry>> {
    conn.rows(
        "SELECT id, ign, player_name, team, role, source FROM roster_entries
         ORDER BY team, ign",
        params![],
    )?
    .iter()
    .map(map)
    .collect()
}

/// The files rosters were imported from, with how many rows each contributed.
pub fn sources(conn: &mut dyn Db) -> Result<Vec<(String, i64)>> {
    // `source` is a plain `TEXT` column, so the collation is named inline here
    // rather than being carried by the column the way `team` and `ign` carry it.
    conn.rows(
        "SELECT source, COUNT(*) FROM roster_entries GROUP BY source ORDER BY source COLLATE nocase",
        params![],
    )?
    .iter()
    .map(|row| Ok((super::at(row, 0)?, super::at(row, 1)?)))
    .collect()
}

pub fn clear(conn: &mut dyn Db, source: Option<&str>) -> Result<usize> {
    let removed = match source {
        Some(source) => conn.exec("DELETE FROM roster_entries WHERE source = $1", params![source])?,
        None => conn.exec("DELETE FROM roster_entries", params![])?,
    };
    Ok(removed as usize)
}

/// Suggestions for a half-typed name, best match first.
///
/// Ranking puts the IGN ahead of the person and the team, so typing "naresh"
/// offers `iQOOS8ULNaresh` before it offers everyone whose team contains the
/// same letters.
pub fn search(conn: &mut dyn Db, query: &str, limit: usize) -> Result<Vec<RosterEntry>> {
    let needle = normalise(query);
    if needle.is_empty() {
        let mut all = list(conn)?;
        all.truncate(limit);
        return Ok(all);
    }
    let mut scored: Vec<(u8, usize, RosterEntry)> = list(conn)?
        .into_iter()
        .filter_map(|entry| rank(&entry, &needle).map(|(tier, offset)| (tier, offset, entry)))
        .collect();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then_with(|| a.2.ign.to_lowercase().cmp(&b.2.ign.to_lowercase()))
    });
    Ok(scored.into_iter().take(limit).map(|(_, _, entry)| entry).collect())
}

/// How well one entry answers `needle`: a tier (lower is better) and where in
/// the field the match starts, or `None` when it does not match at all.
fn rank(entry: &RosterEntry, needle: &str) -> Option<(u8, usize)> {
    let ign = normalise(&entry.ign);
    let player = normalise(&entry.player_name);
    let team = normalise(&entry.team);

    if ign == *needle || player == *needle {
        return Some((0, 0));
    }
    if let Some(at) = ign.find(needle) {
        return Some((if at == 0 { 1 } else { 2 }, at));
    }
    if let Some(at) = player.find(needle) {
        return Some((if at == 0 { 3 } else { 4 }, at));
    }
    if let Some(at) = team.find(needle) {
        return Some((5, at));
    }
    None
}

/// The single entry a typed name resolves to, or `None` when the name is not on
/// any roster or is ambiguous.
///
/// Ambiguity matters: "naresh" matching two players on different teams must not
/// silently file media under whichever sorted first. An exact IGN or player
/// name always wins outright, even when other rows also contain those letters.
pub fn resolve(conn: &mut dyn Db, name: &str) -> Result<Option<RosterEntry>> {
    let needle = normalise(name);
    if needle.is_empty() {
        return Ok(None);
    }
    let entries = list(conn)?;
    let exact: Vec<RosterEntry> = entries
        .iter()
        .filter(|entry| normalise(&entry.ign) == needle || normalise(&entry.player_name) == needle)
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(exact.into_iter().next());
    }
    if exact.len() > 1 {
        return Ok(None);
    }
    let partial: Vec<RosterEntry> = entries
        .into_iter()
        .filter(|entry| rank(entry, &needle).is_some_and(|(tier, _)| tier <= 4))
        .collect();
    if partial.len() == 1 {
        return Ok(partial.into_iter().next());
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn entry(ign: &str, player: &str, team: &str) -> RosterEntry {
        RosterEntry {
            id: 0,
            ign: ign.into(),
            player_name: player.into(),
            team: team.into(),
            role: "player".into(),
            source: String::new(),
        }
    }

    fn seeded() -> Database {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        replace_source(
            &mut conn,
            "bgis.csv",
            &[
                entry("iQOOS8ULNaresh", "Naresh Nallamothu", "iQOO Soul"),
                entry("iQOOS8ULJonathan", "Jonathan Amaral", "iQOO Soul"),
                entry("GodLikeJelly", "Jelly Kumar", "GodLike Esports"),
            ],
        )
        .unwrap();
        drop(conn);
        db
    }

    #[test]
    fn typing_part_of_a_name_suggests_the_in_game_name() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        let hits = search(&mut conn, "naresh", 10).unwrap();
        assert_eq!(hits[0].ign, "iQOOS8ULNaresh");
        assert_eq!(hits[0].team, "iQOO Soul");
    }

    #[test]
    fn separators_and_case_do_not_matter() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        for typed in ["s8ul naresh", "S8UL_NARESH", "iqoos8ulnaresh", "  Naresh  "] {
            let resolved = resolve(&mut conn, typed).unwrap();
            assert_eq!(
                resolved.map(|entry| entry.team),
                Some("iQOO Soul".to_owned()),
                "{typed} should resolve"
            );
        }
    }

    #[test]
    fn a_team_tag_lists_that_whole_team() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        let hits = search(&mut conn, "iqoo", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|entry| entry.team == "iQOO Soul"));
    }

    #[test]
    fn an_ambiguous_name_resolves_to_nobody() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        replace_source(
            &mut conn,
            "second.csv",
            &[entry("XSparkNaresh", "Naresh Kumar", "Team XSpark")],
        )
        .unwrap();
        // Two players called Naresh: SKWAD must ask rather than guess.
        assert!(resolve(&mut conn, "naresh").unwrap().is_none());
        // The full in-game name is still unambiguous.
        assert_eq!(
            resolve(&mut conn, "XSparkNaresh").unwrap().map(|entry| entry.team),
            Some("Team XSpark".to_owned())
        );
    }

    #[test]
    fn a_name_that_is_on_no_roster_resolves_to_nobody() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        assert!(resolve(&mut conn, "Camera operator").unwrap().is_none());
    }

    #[test]
    fn reimporting_a_file_replaces_only_its_own_rows() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        replace_source(&mut conn, "other.csv", &[entry("TXNova", "Nova", "Team Nova")]).unwrap();
        // The corrected BGIS file drops Jonathan and moves Jelly to a new team.
        replace_source(
            &mut conn,
            "bgis.csv",
            &[
                entry("iQOOS8ULNaresh", "Naresh Nallamothu", "iQOO Soul"),
                entry("GodLikeJelly", "Jelly Kumar", "Team XSpark"),
            ],
        )
        .unwrap();

        let all = list(&mut conn).unwrap();
        assert_eq!(all.len(), 3, "the other file's row survives");
        assert!(all.iter().any(|entry| entry.ign == "TXNova"));
        assert!(!all.iter().any(|entry| entry.ign == "iQOOS8ULJonathan"));
        assert_eq!(
            resolve(&mut conn, "GodLikeJelly").unwrap().map(|entry| entry.team),
            Some("Team XSpark".to_owned()),
            "a transfer updates the team"
        );
    }

    #[test]
    fn clearing_one_source_leaves_the_others() {
        let db = seeded();
        let mut conn = db.conn().unwrap();
        replace_source(&mut conn, "other.csv", &[entry("TXNova", "Nova", "Team Nova")]).unwrap();
        assert_eq!(clear(&mut conn, Some("bgis.csv")).unwrap(), 3);
        assert_eq!(list(&mut conn).unwrap().len(), 1);
        assert_eq!(sources(&mut conn).unwrap(), vec![("other.csv".to_owned(), 1)]);
    }

    /// One file listing the same IGN twice is common (a player appears under
    /// two teams in a draft sheet). Postgres refuses a multi-row upsert that
    /// touches one row twice, which is why the import is still row-by-row.
    #[test]
    fn a_file_that_repeats_an_ign_keeps_the_last_line() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        replace_source(
            &mut conn,
            "draft.csv",
            &[
                entry("TXNova", "Nova", "Team Nova"),
                entry("txnova", "Nova Singh", "Team XSpark"),
            ],
        )
        .unwrap();

        let all = list(&mut conn).unwrap();
        assert_eq!(all.len(), 1, "the repeat updated rather than inserted");
        assert_eq!(all[0].team, "Team XSpark");
    }
}
