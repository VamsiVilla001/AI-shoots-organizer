//! The taxonomy: tags, their values, and attaching them to things.
//!
//! A studio keeps a vocabulary — teams, venues, sponsors, match stages — and
//! wants to say it once. Tags here are that vocabulary: a name with every
//! value it has been given, importable from the spreadsheet somebody already
//! maintains and exportable back to one. Attaching a value to a photo, an
//! automatically grouped set of faces or a project collection records it,
//! and from then on the value is offered wherever a tag is typed.
//!
//! Files come in as CSV or JSON, the same two shapes the roster accepts:
//!
//! ```text
//! tag,value            tag,values
//! Team,Gods Reign      Team,"Gods Reign; Velocity"
//! Team,Velocity        Venue,Arena A
//! ```
//!
//! ```json
//! [{ "name": "Team", "values": ["Gods Reign", "Velocity"] }]
//! { "Team": ["Gods Reign", "Velocity"], "Venue": "Arena A" }
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
pub use skwad_database::repo::taxonomy::{AssetTag, TagSuggestion, TagSummary, TaxonomyEntry, TaxonomyImportSummary};
use skwad_database::repo::taxonomy;

use crate::api::{ApiError, Ctx, Result};

const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message)
}

// --- the taxonomy ----------------------------------------------------------------

pub fn list_tags(ctx: &Ctx) -> Result<Vec<TagSummary>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::list(&mut conn)?)
}

/// Creates or extends a tag with the given values.
pub fn save_tag(ctx: &Ctx, name: String, values: Vec<String>) -> Result<TagSummary> {
    let mut conn = ctx.state.db.conn()?;
    let tag_id = taxonomy::upsert_tag(&mut conn, &name)?;
    for value in values.iter().filter(|v| !v.trim().is_empty()) {
        taxonomy::upsert_value(&mut conn, tag_id, value)?;
    }
    taxonomy::get_tag(&mut conn, tag_id)?.ok_or_else(|| ApiError::not_found("the tag vanished while saving"))
}

pub fn rename_tag(ctx: &Ctx, tag_id: i64, name: String) -> Result<TagSummary> {
    let mut conn = ctx.state.db.conn()?;
    taxonomy::rename_tag(&mut conn, tag_id, &name)?;
    taxonomy::get_tag(&mut conn, tag_id)?.ok_or_else(|| ApiError::not_found("no such tag"))
}

/// Removes a tag, its values and every place they were attached.
pub fn delete_tag(ctx: &Ctx, tag_id: i64) -> Result<bool> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::delete_tag(&mut conn, tag_id)?)
}

/// Removes one value from a tag, and from every asset carrying it.
pub fn delete_tag_value(ctx: &Ctx, value_id: i64) -> Result<bool> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::delete_value(&mut conn, value_id)?)
}

// --- assets ----------------------------------------------------------------------

pub fn asset_tags(ctx: &Ctx, kind: String, key: String) -> Result<Vec<AssetTag>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::for_asset(&mut conn, &kind, &key)?)
}

/// Tags for many assets of one kind at once, keyed by asset key.
pub fn assets_tags(ctx: &Ctx, kind: String, keys: Vec<String>) -> Result<HashMap<String, Vec<AssetTag>>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::for_assets(&mut conn, &kind, &keys)?)
}

/// Attaches `tag = value` to an asset, creating the tag and value as needed.
/// Answers with everything now attached, so the UI can replace its list.
pub fn assign_tag(ctx: &Ctx, kind: String, key: String, tag: String, value: String) -> Result<Vec<AssetTag>> {
    let mut conn = ctx.state.db.conn()?;
    taxonomy::assign(&mut conn, &kind, &key, &tag, &value)?;
    Ok(taxonomy::for_asset(&mut conn, &kind, &key)?)
}

/// Attaches one value to several assets — a whole selection at once.
pub fn assign_tag_to_many(ctx: &Ctx, kind: String, keys: Vec<String>, tag: String, value: String) -> Result<usize> {
    let mut conn = ctx.state.db.conn()?;
    let mut count = 0;
    for key in keys.iter().filter(|k| !k.trim().is_empty()) {
        taxonomy::assign(&mut conn, &kind, key, &tag, &value)?;
        count += 1;
    }
    Ok(count)
}

pub fn unassign_tag(ctx: &Ctx, kind: String, key: String, value_id: i64) -> Result<Vec<AssetTag>> {
    let mut conn = ctx.state.db.conn()?;
    taxonomy::unassign(&mut conn, &kind, &key, value_id)?;
    Ok(taxonomy::for_asset(&mut conn, &kind, &key)?)
}

/// What to offer while someone types a value. `tag` narrows it to one tag.
pub fn suggest_tag_values(ctx: &Ctx, tag: Option<String>, query: String, limit: Option<i64>) -> Result<Vec<TagSuggestion>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::suggest(&mut conn, tag.as_deref(), &query, limit.unwrap_or(20))?)
}


// --- groups: a tag on a group is a tag on its files ------------------------------

/// Attaches `tag = value` to an automatic group *and* to every file in it,
/// so the tag is on the images, not only on the card. `group_id` is the
/// album or cluster row; `key` is the stable key the card stores under.
pub fn assign_group_tag(ctx: &Ctx, kind: String, group_id: i64, key: String, tag: String, value: String) -> Result<Vec<AssetTag>> {
    if kind != "album" && kind != "cluster" {
        return Err(bad("only albums and clusters are groups"));
    }
    let mut conn = ctx.state.db.conn()?;
    let media: Vec<String> = taxonomy::group_media_ids(&mut conn, &kind, group_id)?
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    taxonomy::assign(&mut conn, &kind, &key, &tag, &value)?;
    taxonomy::assign_many(&mut conn, "media", &media, &tag, &value)?;
    Ok(taxonomy::for_asset(&mut conn, &kind, &key)?)
}

/// The reverse: detaches the value from the group and from its files.
pub fn unassign_group_tag(ctx: &Ctx, kind: String, group_id: i64, key: String, value_id: i64) -> Result<Vec<AssetTag>> {
    if kind != "album" && kind != "cluster" {
        return Err(bad("only albums and clusters are groups"));
    }
    let mut conn = ctx.state.db.conn()?;
    let media: Vec<String> = taxonomy::group_media_ids(&mut conn, &kind, group_id)?
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    taxonomy::unassign(&mut conn, &kind, &key, value_id)?;
    taxonomy::unassign_many(&mut conn, "media", &media, value_id)?;
    Ok(taxonomy::for_asset(&mut conn, &kind, &key)?)
}

/// Every file carrying a tag value, as media rows — what a collection built
/// from that value is made of.
pub fn media_with_tag(ctx: &Ctx, tag: Option<String>, value: String) -> Result<Vec<skwad_database::models::Media>> {
    let mut conn = ctx.state.db.conn()?;
    let ids = taxonomy::media_ids_with_value(&mut conn, tag.as_deref(), &value)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(row) = skwad_database::repo::media::get_by_id(&mut conn, id)? {
            out.push(row);
        }
    }
    Ok(out)
}

/// Writes every group's tags onto the files currently in the group, across
/// the library or for one collection. Runs by itself after each analysis;
/// this is the same thing on demand, for tags applied before a regroup.
pub fn propagate_group_tags(ctx: &Ctx, shoot_id: Option<i64>) -> Result<usize> {
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::propagate_group_tags(&mut conn, shoot_id)?)
}


// --- import and export -------------------------------------------------------------

/// What an import file turned out to contain, before anything is saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyPreview {
    pub entries: Vec<TaxonomyEntry>,
    pub problems: Vec<String>,
}

/// Reads a CSV or JSON taxonomy from text, without saving it.
pub fn preview_taxonomy_text(_ctx: &Ctx, source: String, text: String) -> Result<TaxonomyPreview> {
    parse(&source, &text)
}

/// Reads and merges a CSV or JSON taxonomy in one step.
pub fn import_taxonomy_text(ctx: &Ctx, source: String, text: String) -> Result<TaxonomyImportSummary> {
    let preview = parse(&source, &text)?;
    if preview.entries.is_empty() {
        return Err(bad(match preview.problems.first() {
            Some(problem) => format!("nothing usable in that file: {problem}"),
            None => "nothing usable in that file".to_string(),
        }));
    }
    let mut conn = ctx.state.db.conn()?;
    Ok(taxonomy::import(&mut conn, &preview.entries)?)
}

/// The whole taxonomy as `csv` or `json` text, for a file the studio keeps.
pub fn export_taxonomy(ctx: &Ctx, format: String) -> Result<String> {
    let mut conn = ctx.state.db.conn()?;
    let tags = taxonomy::list(&mut conn)?;
    match format.trim().to_ascii_lowercase().as_str() {
        "json" => {
            let entries: Vec<TaxonomyEntry> = tags
                .into_iter()
                .map(|tag| TaxonomyEntry {
                    name: tag.name,
                    values: tag.values.into_iter().map(|v| v.value).collect(),
                })
                .collect();
            serde_json::to_string_pretty(&entries).map_err(|e| ApiError::internal(e.to_string()))
        }
        "csv" => {
            let mut out = String::from("tag,value\n");
            for tag in tags {
                if tag.values.is_empty() {
                    out.push_str(&format!("{},\n", csv_cell(&tag.name)));
                }
                for value in tag.values {
                    out.push_str(&format!("{},{}\n", csv_cell(&tag.name), csv_cell(&value.value)));
                }
            }
            Ok(out)
        }
        other => Err(bad(format!("`{other}` is not a format I can write; use csv or json"))),
    }
}

fn csv_cell(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r', ';']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

fn parse(source: &str, text: &str) -> Result<TaxonomyPreview> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(bad("that file is too large to be a tag list"));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let looks_json = source.to_ascii_lowercase().ends_with(".json") || text.trim_start().starts_with(['[', '{']);
    let (entries, problems) = if looks_json { parse_json(text)? } else { parse_csv(text)? };
    Ok(TaxonomyPreview {
        entries: merge(entries),
        problems,
    })
}

/// Folds repeated tag names together and drops empty and duplicate values,
/// keeping first-seen order for both.
fn merge(entries: Vec<TaxonomyEntry>) -> Vec<TaxonomyEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: HashMap<String, TaxonomyEntry> = HashMap::new();
    for entry in entries {
        let name = tidy(&entry.name);
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        let slot = by_name.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            TaxonomyEntry { name, values: Vec::new() }
        });
        for value in entry.values {
            let value = tidy(&value);
            if !value.is_empty() && !slot.values.iter().any(|v| v.eq_ignore_ascii_case(&value)) {
                slot.values.push(value);
            }
        }
    }
    order.into_iter().filter_map(|key| by_name.remove(&key)).collect()
}

fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits a cell holding several values: `A; B; C` or `A | B`. A comma is
/// only a separator when the cell held no other kind, since CSV has already
/// used it for columns.
fn split_values(cell: &str) -> Vec<String> {
    let separator = if cell.contains(';') {
        ';'
    } else if cell.contains('|') {
        '|'
    } else {
        ','
    };
    cell.split(separator).map(tidy).filter(|v| !v.is_empty()).collect()
}

fn parse_csv(text: &str) -> Result<(Vec<TaxonomyEntry>, Vec<String>)> {
    let mut rows = crate::api::roster::split_rows(text);
    if rows.is_empty() {
        return Err(bad("that file is empty"));
    }
    let header: Vec<String> = rows[0].iter().map(|c| tidy(c).to_lowercase()).collect();
    let name_at = header.iter().position(|c| matches!(c.as_str(), "tag" | "name" | "tag name" | "key"));
    let value_at = header
        .iter()
        .position(|c| matches!(c.as_str(), "value" | "values" | "tag value" | "tag values"));
    let has_header = name_at.is_some() || value_at.is_some();
    let (name_at, value_at) = if has_header {
        rows.remove(0);
        (name_at.unwrap_or(0), value_at.unwrap_or(1))
    } else {
        // Two bare columns: tag, value(s).
        (0, 1)
    };

    let mut entries = Vec::new();
    let mut problems = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let line = index + if has_header { 2 } else { 1 };
        let name = row.get(name_at).map(|c| tidy(c)).unwrap_or_default();
        if name.is_empty() {
            problems.push(format!("row {line}: no tag name"));
            continue;
        }
        // A row may carry the value in one column or several values across
        // the rest of the columns — both are how spreadsheets end up.
        let mut values: Vec<String> = Vec::new();
        for (at, cell) in row.iter().enumerate() {
            if at == name_at {
                continue;
            }
            if at == value_at || value_at >= row.len() {
                values.extend(split_values(cell));
            } else if !tidy(cell).is_empty() {
                values.push(tidy(cell));
            }
        }
        entries.push(TaxonomyEntry { name, values });
    }
    Ok((entries, problems))
}

fn parse_json(text: &str) -> Result<(Vec<TaxonomyEntry>, Vec<String>)> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| bad(format!("that JSON could not be read: {error}")))?;
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    let values_of = |v: &serde_json::Value| -> Vec<String> {
        match v {
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::Object(o) => o.get("value").and_then(|v| v.as_str()).map(str::to_string),
                    _ => None,
                })
                .collect(),
            serde_json::Value::String(s) => split_values(s),
            serde_json::Value::Number(n) => vec![n.to_string()],
            _ => Vec::new(),
        }
    };
    match value {
        // [{ "name": "Team", "values": [...] }, …]  or  [{ "tag": …, "value": … }]
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let Some(object) = item.as_object() else {
                    problems.push(format!("item {}: not an object", index + 1));
                    continue;
                };
                let name = object
                    .get("name")
                    .or_else(|| object.get("tag"))
                    .and_then(|v| v.as_str())
                    .map(tidy)
                    .unwrap_or_default();
                if name.is_empty() {
                    problems.push(format!("item {}: no tag name", index + 1));
                    continue;
                }
                let values = object
                    .get("values")
                    .or_else(|| object.get("value"))
                    .map(values_of)
                    .unwrap_or_default();
                entries.push(TaxonomyEntry { name, values });
            }
        }
        // { "Team": [...], "Venue": "Arena A" }
        serde_json::Value::Object(map) => {
            // A wrapper like { "tags": [...] } is unwrapped first.
            if let Some(inner) = map.get("tags").filter(|v| v.is_array()) {
                return parse_json(&inner.to_string());
            }
            for (name, v) in map {
                entries.push(TaxonomyEntry {
                    name: tidy(&name),
                    values: values_of(&v),
                });
            }
        }
        _ => return Err(bad("expected a JSON array of tags or an object of tag names to values")),
    }
    Ok((entries, problems))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_with_a_header_and_multi_value_cells_is_read() {
        let preview = parse("tags.csv", "tag,values\nTeam,\"Gods Reign; Velocity\"\nteam,Soul\nVenue,Arena A\n,orphan\n").unwrap();
        assert_eq!(preview.entries.len(), 2, "{:?}", preview.entries);
        assert_eq!(preview.entries[0].name, "Team");
        assert_eq!(preview.entries[0].values, ["Gods Reign", "Velocity", "Soul"]);
        assert_eq!(preview.entries[1].values, ["Arena A"]);
        assert_eq!(preview.problems.len(), 1);
    }

    #[test]
    fn csv_without_a_header_is_two_columns_and_spread_values_count() {
        let preview = parse("x.csv", "Team,A,B,C\nStage,Final\n").unwrap();
        assert_eq!(preview.entries[0].values, ["A", "B", "C"]);
        assert_eq!(preview.entries[1].values, ["Final"]);
    }

    #[test]
    fn json_accepts_both_shapes() {
        let list = parse("t.json", r#"[{"name":"Team","values":["A","a","B"]},{"tag":"Venue","value":"Hall"}]"#).unwrap();
        assert_eq!(list.entries[0].values, ["A", "B"], "case-duplicates folded");
        assert_eq!(list.entries[1].values, ["Hall"]);
        let map = parse("t.json", r#"{"Team":["A"],"Venue":"Hall; Arena"}"#).unwrap();
        assert_eq!(map.entries.len(), 2);
        assert_eq!(map.entries.iter().find(|e| e.name == "Venue").unwrap().values, ["Hall", "Arena"]);
        let wrapped = parse("t.json", r#"{"tags":[{"name":"X","values":[1,2]}]}"#).unwrap();
        assert_eq!(wrapped.entries[0].values, ["1", "2"]);
        assert!(parse("t.json", "42").is_err());
    }

    #[test]
    fn csv_export_quotes_what_needs_quoting() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("Soul, Inc."), "\"Soul, Inc.\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
