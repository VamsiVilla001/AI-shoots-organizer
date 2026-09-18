//! Importing team rosters, and answering "who is this and what team are they
//! on?" while a reviewer types a name.
//!
//! An event's roster arrives as the spreadsheet somebody already keeps: team,
//! player, in-game name. SKWAD reads it, and from then on naming one face is
//! enough to file that player's media under their team — the reviewer does the
//! identifying they were doing anyway, and the roster supplies the bookkeeping.
//!
//! Both CSV and JSON are accepted because both are what people actually have.

use std::path::Path;

use serde::{Deserialize, Serialize};
use skwad_database::repo::roster::{self, RosterEntry};

use crate::api::{ApiError as CommandError, Ctx, Result};

/// A roster file larger than this is not a roster.
const MAX_ROSTER_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

/// What a file turned out to contain, before anything is saved.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterPreview {
    /// The file name, which is also the key rows are stored under.
    pub source: String,
    pub entries: Vec<RosterEntry>,
    pub teams: Vec<String>,
    /// Rows that could not be read, with the reason. Shown to the user rather
    /// than swallowed: a roster with three unreadable rows is worth fixing.
    pub problems: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterSummary {
    pub entries: usize,
    pub teams: Vec<String>,
    pub sources: Vec<RosterSource>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterSource {
    pub source: String,
    pub entries: i64,
}

/// One row as it arrives from a file, before it is cleaned up.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    #[serde(alias = "IGN", alias = "inGameName", alias = "in_game_name", alias = "gamerTag")]
    ign: Option<String>,
    #[serde(alias = "player", alias = "playerName", alias = "player_name", alias = "name")]
    player: Option<String>,
    #[serde(alias = "team", alias = "teamName", alias = "team_name")]
    team: Option<String>,
    #[serde(alias = "role")]
    role: Option<String>,
}

// --- commands -------------------------------------------------------------

/// Reads a roster file and reports what is in it. Nothing is saved yet, so the
/// user can look at the teams before committing to them.
pub fn preview_roster_file(_ctx: &Ctx, path: String) -> Result<RosterPreview> {
    let path = Path::new(&path);
    let size = std::fs::metadata(path)
        .map_err(|error| command_error(format!("could not open that file: {error}")))?
        .len();
    if size > MAX_ROSTER_BYTES {
        return Err(command_error("that file is too large to be a roster"));
    }
    let text =
        std::fs::read_to_string(path).map_err(|error| command_error(format!("could not read that file: {error}")))?;
    let source = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "roster".to_owned());

    let is_json = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        || text.trim_start().starts_with(['{', '[']);
    let (entries, problems) = if is_json { parse_json(&text)? } else { parse_csv(&text)? };

    if entries.is_empty() {
        return Err(command_error(format!(
            "no usable rows in that file — expected team, player and in-game name columns{}",
            first_problem(&problems)
        )));
    }

    let mut teams: Vec<String> = entries.iter().map(|entry| entry.team.clone()).collect();
    teams.sort_by_key(|team| team.to_lowercase());
    teams.dedup_by_key(|team| team.to_lowercase());

    Ok(RosterPreview {
        source,
        entries,
        teams,
        problems,
    })
}

/// Saves a previewed roster, replacing whatever the same file gave last time.
pub fn import_roster(
    ctx: &Ctx,
    source: String,
    entries: Vec<RosterEntry>,
) -> Result<RosterSummary> {
    let source = source.trim().to_owned();
    if source.is_empty() {
        return Err(command_error("the roster needs a file name to be stored under"));
    }
    if entries.is_empty() {
        return Err(command_error("that roster has no rows"));
    }
    if entries.len() > MAX_ENTRIES {
        return Err(command_error("that roster has more rows than SKWAD will import"));
    }
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    roster::replace_source(&mut conn, &source, &entries).map_err(command_error)?;
    summary(&mut conn)
}

pub fn roster_summary(ctx: &Ctx) -> Result<RosterSummary> {
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    summary(&mut conn)
}

pub fn list_roster(ctx: &Ctx) -> Result<Vec<RosterEntry>> {
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    roster::list(&mut conn).map_err(command_error)
}

/// Suggestions for a half-typed name. This is what turns "naresh" into
/// "iQOOS8ULNaresh · iQOO Soul" in the naming field.
pub fn search_roster(ctx: &Ctx, query: String, limit: Option<usize>) -> Result<Vec<RosterEntry>> {
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    roster::search(&mut conn, &query, limit.unwrap_or(8).clamp(1, 50)).map_err(command_error)
}

/// The team a typed name belongs to, or nothing when the roster cannot say for
/// certain. Callers treat `None` as "leave the team alone".
pub fn resolve_roster_name(ctx: &Ctx, name: String) -> Result<Option<RosterEntry>> {
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    roster::resolve(&mut conn, &name).map_err(command_error)
}

pub fn clear_roster(ctx: &Ctx, source: Option<String>) -> Result<RosterSummary> {
    let mut conn = ctx.state.db.conn().map_err(command_error)?;
    roster::clear(&mut conn, source.as_deref()).map_err(command_error)?;
    summary(&mut conn)
}

fn summary(conn: &mut dyn skwad_database::Db) -> Result<RosterSummary> {
    let entries = roster::list(conn).map_err(command_error)?;
    let mut teams: Vec<String> = entries.iter().map(|entry| entry.team.clone()).collect();
    teams.sort_by_key(|team| team.to_lowercase());
    teams.dedup_by_key(|team| team.to_lowercase());
    let sources = roster::sources(conn)
        .map_err(command_error)?
        .into_iter()
        .map(|(source, entries)| RosterSource { source, entries })
        .collect();
    Ok(RosterSummary {
        entries: entries.len(),
        teams,
        sources,
    })
}

fn first_problem(problems: &[String]) -> String {
    match problems.first() {
        Some(problem) => format!(". {problem}"),
        None => String::new(),
    }
}

// --- parsing --------------------------------------------------------------

/// Accepts either a flat array of rows, or a team-per-object shape:
///
/// ```json
/// [{ "team": "iQOO Soul", "players": [{ "ign": "…", "name": "…" }] }]
/// ```
fn parse_json(text: &str) -> Result<(Vec<RosterEntry>, Vec<String>)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TeamBlock {
        #[serde(alias = "team", alias = "teamName", alias = "name")]
        team: String,
        players: Vec<RawEntry>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    // Order matters: every field of RawEntry is optional, so a team block
    // would happily parse as a flat row. The stricter shape is tried first.
    enum Shape {
        Teams(Vec<TeamBlock>),
        Wrapped { players: Vec<RawEntry> },
        Flat(Vec<RawEntry>),
    }

    let shape: Shape =
        serde_json::from_str(text).map_err(|error| command_error(format!("that JSON could not be read: {error}")))?;
    let rows: Vec<(RawEntry, Option<String>)> = match shape {
        Shape::Flat(rows) | Shape::Wrapped { players: rows } => rows.into_iter().map(|row| (row, None)).collect(),
        Shape::Teams(blocks) => blocks
            .into_iter()
            .flat_map(|block| {
                let team = block.team;
                block
                    .players
                    .into_iter()
                    .map(move |player| (player, Some(team.clone())))
            })
            .collect(),
    };

    let mut entries = Vec::new();
    let mut problems = Vec::new();
    for (index, (raw, team)) in rows.into_iter().enumerate() {
        match clean(raw, team, index + 1) {
            Ok(entry) => entries.push(entry),
            Err(problem) => problems.push(problem),
        }
    }
    Ok((dedupe(entries, &mut problems), problems))
}

fn parse_csv(text: &str) -> Result<(Vec<RosterEntry>, Vec<String>)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = split_rows(text);
    if rows.is_empty() {
        return Err(command_error("that file is empty"));
    }
    let header = rows.remove(0);
    let columns: Vec<String> = header.iter().map(|cell| roster::normalise(cell)).collect();
    let find = |names: &[&str]| -> Option<usize> {
        columns
            .iter()
            .position(|column| names.iter().any(|name| column == name))
    };
    let ign_at = find(&["ign", "ingamename", "gamertag", "gamename", "nickname"]);
    let player_at = find(&["player", "playername", "name", "realname", "fullname"]);
    let team_at = find(&["team", "teamname", "squad", "organisation", "organization"]);
    let role_at = find(&["role", "position", "type"]);

    if team_at.is_none() || (ign_at.is_none() && player_at.is_none()) {
        return Err(command_error(
            "expected a header row with a team column and an in-game name (or player) column",
        ));
    }

    let cell = |row: &[String], at: Option<usize>| at.and_then(|at| row.get(at)).cloned();
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        let raw = RawEntry {
            ign: cell(row, ign_at),
            player: cell(row, player_at),
            team: cell(row, team_at),
            role: cell(row, role_at),
        };
        // +2: one for the header, one because people count rows from 1.
        match clean(raw, None, index + 2) {
            Ok(entry) => entries.push(entry),
            Err(problem) => problems.push(problem),
        }
    }
    Ok((dedupe(entries, &mut problems), problems))
}

/// Splits CSV text into rows of cells, honouring quoted fields that contain
/// commas, quotes and line breaks — a team called `Soul, Inc.` is one cell.
fn split_rows(text: &str) -> Vec<Vec<String>> {
    // Spreadsheets in several European locales export with semicolons; pick
    // whichever separator the first line actually uses.
    let first_line = text.lines().next().unwrap_or("");
    let separator = if first_line.matches(';').count() > first_line.matches(',').count() {
        ';'
    } else {
        ','
    };

    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == separator && !quoted => row.push(std::mem::take(&mut cell)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            c => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows.retain(|row| !row.iter().all(|cell| cell.trim().is_empty()));
    rows
}

/// Turns a raw row into an entry, or explains why it cannot.
fn clean(raw: RawEntry, team_override: Option<String>, line: usize) -> std::result::Result<RosterEntry, String> {
    let trim = |value: Option<String>| value.map(|value| value.trim().to_owned()).unwrap_or_default();
    let ign = trim(raw.ign);
    let player = trim(raw.player);
    let team = team_override
        .map(|team| team.trim().to_owned())
        .unwrap_or_else(|| trim(raw.team));
    let role = {
        let role = trim(raw.role).to_lowercase();
        if role.is_empty() {
            "player".to_owned()
        } else {
            role
        }
    };

    if team.is_empty() {
        return Err(format!("row {line}: no team"));
    }
    // A row with only a real name still works: that name becomes the key the
    // reviewer types, which is exactly what happens for coaches and staff.
    let ign = if ign.is_empty() { player.clone() } else { ign };
    if ign.is_empty() {
        return Err(format!("row {line}: no in-game name or player name"));
    }
    if ign.chars().count() > 120 || player.chars().count() > 120 || team.chars().count() > 120 {
        return Err(format!("row {line}: a value is longer than 120 characters"));
    }

    Ok(RosterEntry {
        id: 0,
        ign,
        player_name: player,
        team,
        role,
        source: String::new(),
    })
}

/// Keeps the first row for each in-game name and reports the rest, so a file
/// listing a player twice imports cleanly and says so.
fn dedupe(entries: Vec<RosterEntry>, problems: &mut Vec<String>) -> Vec<RosterEntry> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        if seen.insert(roster::normalise(&entry.ign)) {
            kept.push(entry);
        } else {
            problems.push(format!("{} appears more than once; the first row was used", entry.ign));
        }
    }
    kept
}

fn command_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::bad_request(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(text: &str) -> (Vec<RosterEntry>, Vec<String>) {
        parse_csv(text).unwrap()
    }

    #[test]
    fn a_plain_csv_imports() {
        let (entries, problems) =
            csv("team,player,ign\niQOO Soul,Naresh Nallamothu,iQOOS8ULNaresh\nGodLike,Jelly,GodLikeJelly\n");
        assert!(problems.is_empty());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].ign, "iQOOS8ULNaresh");
        assert_eq!(entries[0].player_name, "Naresh Nallamothu");
        assert_eq!(entries[0].team, "iQOO Soul");
        assert_eq!(entries[0].role, "player");
    }

    #[test]
    fn columns_may_be_in_any_order_and_spelled_differently() {
        let (entries, _) = csv("In-Game Name,Team Name,Player Name,Role\nTXNova,Team XSpark,Nova Kumar,coach\n");
        assert_eq!(entries[0].ign, "TXNova");
        assert_eq!(entries[0].team, "Team XSpark");
        assert_eq!(entries[0].role, "coach");
    }

    #[test]
    fn quoted_cells_keep_their_commas() {
        let (entries, _) = csv("team,player,ign\n\"Soul, Inc.\",\"Nallamothu, Naresh\",SoulNaresh\n");
        assert_eq!(entries[0].team, "Soul, Inc.");
        assert_eq!(entries[0].player_name, "Nallamothu, Naresh");
    }

    #[test]
    fn a_byte_order_mark_and_crlf_from_excel_are_tolerated() {
        let (entries, problems) = csv("\u{feff}team,player,ign\r\niQOO Soul,Naresh,iQOOS8ULNaresh\r\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].team, "iQOO Soul");
    }

    #[test]
    fn semicolon_exports_are_read_too() {
        let (entries, _) = csv("team;player;ign\niQOO Soul;Naresh;iQOOS8ULNaresh\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ign, "iQOOS8ULNaresh");
    }

    #[test]
    fn a_row_without_a_team_is_reported_not_silently_dropped() {
        let (entries, problems) = csv("team,player,ign\n,Naresh,iQOOS8ULNaresh\niQOO Soul,Jelly,SoulJelly\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(problems, vec!["row 2: no team"]);
    }

    #[test]
    fn a_player_listed_twice_is_imported_once_and_reported() {
        let (entries, problems) =
            csv("team,player,ign\niQOO Soul,Naresh,SoulNaresh\niQOO Soul,Naresh again,soulnaresh\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("more than once"));
    }

    #[test]
    fn a_missing_ign_falls_back_to_the_player_name() {
        let (entries, _) = csv("team,player,ign\niQOO Soul,Coach Ramesh,\n");
        assert_eq!(entries[0].ign, "Coach Ramesh");
        assert_eq!(entries[0].player_name, "Coach Ramesh");
    }

    #[test]
    fn a_file_without_the_expected_columns_says_so() {
        let error = parse_csv("first,second\na,b\n").unwrap_err();
        assert!(error.message.contains("team column"), "{}", error.message);
    }

    #[test]
    fn a_flat_json_array_imports() {
        let (entries, problems) = parse_json(
            r#"[{"ign":"iQOOS8ULNaresh","player":"Naresh","team":"iQOO Soul"},
                {"inGameName":"GodLikeJelly","name":"Jelly","teamName":"GodLike"}]"#,
        )
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].ign, "GodLikeJelly");
        assert_eq!(entries[1].team, "GodLike");
    }

    #[test]
    fn a_team_grouped_json_file_imports() {
        let (entries, _) = parse_json(
            r#"[{"team":"iQOO Soul","players":[{"ign":"iQOOS8ULNaresh","name":"Naresh"},
                                               {"ign":"iQOOS8ULJonathan","name":"Jonathan"}]}]"#,
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.team == "iQOO Soul"));
    }

    #[test]
    fn broken_json_is_reported_clearly() {
        let error = parse_json("{ not json").unwrap_err();
        assert!(error.message.contains("could not be read"), "{}", error.message);
    }
}
