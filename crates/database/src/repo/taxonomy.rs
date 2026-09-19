//! Tags, their recorded values, and the assets they are attached to.
//!
//! See `sql/006_taxonomy.sql` for the shape. Everything here is keyed by
//! *names* on the way in — a caller says "Team = Gods Reign", not "value 41"
//! — because that is how a person types it, and the tag and value rows are
//! created on demand. Ids come back out so the UI can rename and delete.

use std::collections::HashMap;

use postgres::Row;
use serde::{Deserialize, Serialize};

use super::get;
use crate::client::Db;
use crate::{now, params, DbError, Result};

/// The kinds of thing a tag may be attached to.
pub const ASSET_KINDS: &[&str] = &["media", "album", "cluster", "collection"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TagValue {
    pub id: i64,
    pub value: String,
    /// How many assets carry this value.
    pub uses: i64,
}

/// One tag with everything it has been given, for the taxonomy list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TagSummary {
    pub id: i64,
    pub name: String,
    pub values: Vec<TagValue>,
    pub created_at: String,
    pub updated_at: String,
}

/// One assignment as an asset sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssetTag {
    pub tag_id: i64,
    pub tag: String,
    pub value_id: i64,
    pub value: String,
}

/// A value offered while someone types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TagSuggestion {
    pub tag_id: i64,
    pub tag: String,
    pub value_id: i64,
    pub value: String,
    pub uses: i64,
}

fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_kind(kind: &str) -> Result<()> {
    if ASSET_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(DbError::other(format!("`{kind}` is not something a tag can be attached to")))
    }
}

fn check_name(name: &str) -> Result<String> {
    let cleaned = clean(name);
    if cleaned.is_empty() {
        return Err(DbError::other("a tag needs a name"));
    }
    if cleaned.len() > 120 {
        return Err(DbError::other("tag names are limited to 120 characters"));
    }
    Ok(cleaned)
}

fn check_value(value: &str) -> Result<String> {
    let cleaned = clean(value);
    if cleaned.is_empty() {
        return Err(DbError::other("a tag value cannot be empty"));
    }
    if cleaned.len() > 500 {
        return Err(DbError::other("tag values are limited to 500 characters"));
    }
    Ok(cleaned)
}

// --- tags and values ------------------------------------------------------------

/// Creates the tag if it does not exist. Names are matched without regard
/// to case, so "team" finds "Team" and does not make a second one.
pub fn upsert_tag(conn: &mut dyn Db, name: &str) -> Result<i64> {
    let name = check_name(name)?;
    let stamp = now();
    let row = conn.row_one(
        "INSERT INTO tags (name, created_at, updated_at) VALUES ($1, $2, $2)
         ON CONFLICT (name) DO UPDATE SET updated_at = excluded.updated_at
         RETURNING id",
        params![name, stamp],
    )?;
    get(&row, "id")
}

/// Records a value for a tag, creating it if needed. Idempotent.
pub fn upsert_value(conn: &mut dyn Db, tag_id: i64, value: &str) -> Result<i64> {
    let value = check_value(value)?;
    let row = conn.row_one(
        "INSERT INTO tag_values (tag_id, value, created_at) VALUES ($1, $2, $3)
         ON CONFLICT (tag_id, value) DO UPDATE SET value = tag_values.value
         RETURNING id",
        params![tag_id, value, now()],
    )?;
    get(&row, "id")
}

pub fn rename_tag(conn: &mut dyn Db, tag_id: i64, name: &str) -> Result<()> {
    let name = check_name(name)?;
    let n = conn.exec(
        "UPDATE tags SET name = $2, updated_at = $3 WHERE id = $1",
        params![tag_id, name, now()],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!("tag {tag_id}")));
    }
    Ok(())
}

pub fn delete_tag(conn: &mut dyn Db, tag_id: i64) -> Result<bool> {
    Ok(conn.exec("DELETE FROM tags WHERE id = $1", params![tag_id])? == 1)
}

pub fn delete_value(conn: &mut dyn Db, value_id: i64) -> Result<bool> {
    Ok(conn.exec("DELETE FROM tag_values WHERE id = $1", params![value_id])? == 1)
}

fn map_summary_rows(rows: &[Row]) -> Result<Vec<TagSummary>> {
    let mut out: Vec<TagSummary> = Vec::new();
    for row in rows {
        let id: i64 = get(row, "id")?;
        if out.last().map(|t| t.id) != Some(id) {
            out.push(TagSummary {
                id,
                name: get(row, "name")?,
                values: Vec::new(),
                created_at: get(row, "created_at")?,
                updated_at: get(row, "updated_at")?,
            });
        }
        let value_id: Option<i64> = get(row, "value_id")?;
        if let Some(value_id) = value_id {
            out.last_mut().expect("pushed above").values.push(TagValue {
                id: value_id,
                value: get(row, "value")?,
                uses: get(row, "uses")?,
            });
        }
    }
    Ok(out)
}

const SUMMARY_SQL: &str = "SELECT t.id, t.name, t.created_at, t.updated_at,
            v.id AS value_id, v.value,
            (SELECT COUNT(*) FROM asset_tags a WHERE a.tag_value_id = v.id) AS uses
       FROM tags t
       LEFT JOIN tag_values v ON v.tag_id = t.id";

/// Every tag with its values, alphabetically.
pub fn list(conn: &mut dyn Db) -> Result<Vec<TagSummary>> {
    let rows = conn.rows(&format!("{SUMMARY_SQL} ORDER BY t.name, t.id, v.value"), params![])?;
    map_summary_rows(&rows)
}

pub fn get_tag(conn: &mut dyn Db, tag_id: i64) -> Result<Option<TagSummary>> {
    let rows = conn.rows(
        &format!("{SUMMARY_SQL} WHERE t.id = $1 ORDER BY t.id, v.value"),
        params![tag_id],
    )?;
    Ok(map_summary_rows(&rows)?.into_iter().next())
}

// --- assignments --------------------------------------------------------------

/// Attaches `tag = value` to an asset, creating the tag and value as needed.
/// Attaching the same pair twice is a no-op.
pub fn assign(conn: &mut dyn Db, kind: &str, key: &str, tag: &str, value: &str) -> Result<()> {
    check_kind(kind)?;
    if key.trim().is_empty() {
        return Err(DbError::other("the asset key is empty"));
    }
    let tag_id = upsert_tag(conn, tag)?;
    let value_id = upsert_value(conn, tag_id, value)?;
    conn.exec(
        "INSERT INTO asset_tags (tag_value_id, asset_kind, asset_key, created_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
        params![value_id, kind, key, now()],
    )?;
    Ok(())
}

/// Detaches one value from an asset. The value itself stays in the taxonomy.
pub fn unassign(conn: &mut dyn Db, kind: &str, key: &str, value_id: i64) -> Result<bool> {
    check_kind(kind)?;
    Ok(conn.exec(
        "DELETE FROM asset_tags WHERE tag_value_id = $1 AND asset_kind = $2 AND asset_key = $3",
        params![value_id, kind, key],
    )? == 1)
}

fn map_asset_tag(row: &Row) -> Result<AssetTag> {
    Ok(AssetTag {
        tag_id: get(row, "tag_id")?,
        tag: get(row, "tag")?,
        value_id: get(row, "value_id")?,
        value: get(row, "value")?,
    })
}

const ASSET_SQL: &str = "SELECT a.asset_key, t.id AS tag_id, t.name AS tag, v.id AS value_id, v.value
       FROM asset_tags a
       JOIN tag_values v ON v.id = a.tag_value_id
       JOIN tags t ON t.id = v.tag_id";

/// Everything attached to one asset, ordered by tag then value.
pub fn for_asset(conn: &mut dyn Db, kind: &str, key: &str) -> Result<Vec<AssetTag>> {
    check_kind(kind)?;
    conn.rows(
        &format!("{ASSET_SQL} WHERE a.asset_kind = $1 AND a.asset_key = $2 ORDER BY t.name, v.value"),
        params![kind, key],
    )?
    .iter()
    .map(map_asset_tag)
    .collect()
}

/// The same for many assets at once — what a grid of cards asks for.
pub fn for_assets(conn: &mut dyn Db, kind: &str, keys: &[String]) -> Result<HashMap<String, Vec<AssetTag>>> {
    check_kind(kind)?;
    let mut out: HashMap<String, Vec<AssetTag>> = HashMap::new();
    if keys.is_empty() {
        return Ok(out);
    }
    let rows = conn.rows(
        &format!("{ASSET_SQL} WHERE a.asset_kind = $1 AND a.asset_key = ANY($2) ORDER BY a.asset_key, t.name, v.value"),
        params![kind, keys],
    )?;
    for row in &rows {
        let key: String = get(row, "asset_key")?;
        out.entry(key).or_default().push(map_asset_tag(row)?);
    }
    Ok(out)
}

/// Values matching what someone has typed so far, most used first. With a
/// tag named, only that tag's values; otherwise across every tag.
pub fn suggest(conn: &mut dyn Db, tag: Option<&str>, query: &str, limit: i64) -> Result<Vec<TagSuggestion>> {
    let pattern = format!("%{}%", clean(query));
    let limit = limit.clamp(1, 200);
    let rows = match tag.map(clean).filter(|t| !t.is_empty()) {
        Some(tag) => conn.rows(
            "SELECT t.id AS tag_id, t.name AS tag, v.id AS value_id, v.value,
                    (SELECT COUNT(*) FROM asset_tags a WHERE a.tag_value_id = v.id) AS uses
               FROM tag_values v JOIN tags t ON t.id = v.tag_id
              WHERE t.name = $1 AND (v.value COLLATE ucs_basic) ILIKE $2
              ORDER BY uses DESC, v.value
              LIMIT $3",
            params![tag, pattern, limit],
        )?,
        None => conn.rows(
            "SELECT t.id AS tag_id, t.name AS tag, v.id AS value_id, v.value,
                    (SELECT COUNT(*) FROM asset_tags a WHERE a.tag_value_id = v.id) AS uses
               FROM tag_values v JOIN tags t ON t.id = v.tag_id
              WHERE (v.value COLLATE ucs_basic) ILIKE $1 OR (t.name COLLATE ucs_basic) ILIKE $1
              ORDER BY uses DESC, t.name, v.value
              LIMIT $2",
            params![pattern, limit],
        )?,
    };
    rows.iter()
        .map(|row| {
            Ok(TagSuggestion {
                tag_id: get(row, "tag_id")?,
                tag: get(row, "tag")?,
                value_id: get(row, "value_id")?,
                value: get(row, "value")?,
                uses: get(row, "uses")?,
            })
        })
        .collect()
}

/// The `WHERE` fragment that restricts a media query to rows carrying a tag
/// value. `tag` may be `None` to match the value under any tag. The two
/// parameters are appended by the caller in this order: tag (or empty), value.
pub fn media_filter_sql(tag_param: usize, value_param: usize) -> String {
    format!(
        "EXISTS (SELECT 1 FROM asset_tags a
                   JOIN tag_values v ON v.id = a.tag_value_id
                   JOIN tags t ON t.id = v.tag_id
                  WHERE a.asset_kind = 'media' AND a.asset_key = m.id::text
                    AND v.value = ${value_param}
                    AND (${tag_param} = '' OR t.name = ${tag_param}))"
    )
}

/// One row of an import: a tag and the values to record for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyEntry {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyImportSummary {
    pub tags_created: usize,
    pub values_created: usize,
    pub tags_seen: usize,
    pub values_seen: usize,
}

/// Merges entries into the taxonomy. Existing tags and values are kept;
/// nothing is removed by an import.
pub fn import(conn: &mut dyn Db, entries: &[TaxonomyEntry]) -> Result<TaxonomyImportSummary> {
    let mut summary = TaxonomyImportSummary::default();
    for entry in entries {
        let Ok(name) = check_name(&entry.name) else { continue };
        let existed: Option<i64> = conn
            .row_opt("SELECT id FROM tags WHERE name = $1", params![name])?
            .map(|r| get(&r, "id"))
            .transpose()?;
        let tag_id = match existed {
            Some(id) => id,
            None => {
                summary.tags_created += 1;
                upsert_tag(conn, &name)?
            }
        };
        summary.tags_seen += 1;
        for value in &entry.values {
            let Ok(value) = check_value(value) else { continue };
            let had: bool = conn
                .row_opt(
                    "SELECT 1 FROM tag_values WHERE tag_id = $1 AND value = $2",
                    params![tag_id, value],
                )?
                .is_some();
            if !had {
                summary.values_created += 1;
            }
            upsert_value(conn, tag_id, &value)?;
            summary.values_seen += 1;
        }
    }
    Ok(summary)
}

// --- groups: a tag on a group is a tag on its files -------------------------------

/// The media ids an automatic group is made of: an album's rows, or every
/// file a cluster's faces appear in.
pub fn group_media_ids(conn: &mut dyn Db, kind: &str, group_id: i64) -> Result<Vec<i64>> {
    let sql = match kind {
        "album" => "SELECT media_id FROM album_media WHERE album_id = $1",
        "cluster" => "SELECT DISTINCT media_id FROM faces WHERE cluster_id = $1",
        other => return Err(DbError::other(format!("`{other}` is not a group kind"))),
    };
    conn.rows(sql, params![group_id])?
        .iter()
        .map(|row| super::at(row, 0))
        .collect()
}

/// Attaches one value to many assets of one kind in a single statement.
pub fn assign_many(conn: &mut dyn Db, kind: &str, keys: &[String], tag: &str, value: &str) -> Result<usize> {
    check_kind(kind)?;
    if keys.is_empty() {
        return Ok(0);
    }
    let tag_id = upsert_tag(conn, tag)?;
    let value_id = upsert_value(conn, tag_id, value)?;
    let n = conn.exec(
        "INSERT INTO asset_tags (tag_value_id, asset_kind, asset_key, created_at)
         SELECT $1, $2, k, $4 FROM unnest($3::text[]) AS k
         ON CONFLICT DO NOTHING",
        params![value_id, kind, keys, now()],
    )?;
    Ok(n as usize)
}

/// Detaches one value from many assets of one kind.
pub fn unassign_many(conn: &mut dyn Db, kind: &str, keys: &[String], value_id: i64) -> Result<usize> {
    check_kind(kind)?;
    if keys.is_empty() {
        return Ok(0);
    }
    let n = conn.exec(
        "DELETE FROM asset_tags WHERE tag_value_id = $1 AND asset_kind = $2 AND asset_key = ANY($3)",
        params![value_id, kind, keys],
    )?;
    Ok(n as usize)
}

/// Media ids carrying `value` (under `tag`, or any tag when `None`) — what
/// a collection built from a tag is made of.
pub fn media_ids_with_value(conn: &mut dyn Db, tag: Option<&str>, value: &str) -> Result<Vec<i64>> {
    let value = clean(value);
    let tag = tag.map(clean).unwrap_or_default();
    conn.rows(
        "SELECT DISTINCT a.asset_key::bigint AS media_id
           FROM asset_tags a
           JOIN tag_values v ON v.id = a.tag_value_id
           JOIN tags t ON t.id = v.tag_id
          WHERE a.asset_kind = 'media' AND v.value = $2 AND ($1 = '' OR t.name = $1)
          ORDER BY media_id",
        params![tag, value],
    )?
    .iter()
    .map(|row| super::at(row, 0))
    .collect()
}

// --- keeping group tags on the files ----------------------------------------------

/// Resolves the key a group's tags are stored under (see `albumTagKey` in
/// the front end) to the group's current row, if it still exists.
///
/// `shoot:S/cluster:ID` names a cluster; `shoot:S/person:P`,
/// `shoot:S/persons:A+B` and `shoot:S/{type}:{name}` name an album by what
/// it is, which is why an album keeps its tags across a regeneration.
pub fn resolve_group_key(conn: &mut dyn Db, kind: &str, key: &str) -> Result<Option<i64>> {
    let Some((shoot_part, rest)) = key.split_once('/') else { return Ok(None) };
    let Some(shoot_id) = shoot_part.strip_prefix("shoot:").and_then(|s| s.parse::<i64>().ok()) else {
        return Ok(None);
    };
    let Some((selector, target)) = rest.split_once(':') else { return Ok(None) };
    let row = match (kind, selector) {
        ("cluster", "cluster") => {
            let Ok(id) = target.parse::<i64>() else { return Ok(None) };
            conn.row_opt("SELECT id FROM clusters WHERE id = $1 AND shoot_id = $2", params![id, shoot_id])?
        }
        ("album", "person") => {
            let Ok(person) = target.parse::<i64>() else { return Ok(None) };
            conn.row_opt(
                "SELECT id FROM albums WHERE shoot_id = $1 AND album_type = 'player'
                    AND person_ids IS NOT NULL AND person_ids::jsonb @> to_jsonb(ARRAY[$2::bigint])
                  ORDER BY id LIMIT 1",
                params![shoot_id, person],
            )?
        }
        ("album", "persons") => {
            let mut ids: Vec<i64> = target.split('+').filter_map(|p| p.parse().ok()).collect();
            ids.sort_unstable();
            conn.row_opt(
                "SELECT id FROM albums WHERE shoot_id = $1 AND album_type = 'multiPlayer'
                    AND person_ids IS NOT NULL
                    AND (SELECT array_agg(v::bigint ORDER BY v::bigint) FROM jsonb_array_elements_text(person_ids::jsonb) v) = $2::bigint[]
                  ORDER BY id LIMIT 1",
                params![shoot_id, ids],
            )?
        }
        ("album", album_type) => conn.row_opt(
            "SELECT id FROM albums WHERE shoot_id = $1 AND album_type = $2 AND name = $3 ORDER BY id LIMIT 1",
            params![shoot_id, album_type, target],
        )?,
        _ => None,
    };
    row.map(|r| super::at(&r, 0)).transpose()
}

/// Every group assignment as (kind, key, value id, tag, value).
fn group_assignments(conn: &mut dyn Db, shoot_id: Option<i64>) -> Result<Vec<(String, String, i64, String, String)>> {
    let prefix = shoot_id.map(|id| format!("shoot:{id}/")).unwrap_or_default();
    conn.rows(
        "SELECT a.asset_kind, a.asset_key, v.id AS value_id, t.name AS tag, v.value
           FROM asset_tags a
           JOIN tag_values v ON v.id = a.tag_value_id
           JOIN tags t ON t.id = v.tag_id
          WHERE a.asset_kind IN ('album', 'cluster')
            AND ($1 = '' OR a.asset_key LIKE $1 || '%')",
        params![prefix],
    )?
    .iter()
    .map(|row| {
        Ok((
            get(row, "asset_kind")?,
            get(row, "asset_key")?,
            get(row, "value_id")?,
            get(row, "tag")?,
            get(row, "value")?,
        ))
    })
    .collect()
}

/// Writes every group's tags onto the files currently in that group.
///
/// Membership moves — faces get named, clusters merge, albums are rebuilt —
/// so this runs after clustering and album generation as well as on demand,
/// and it only ever adds: a file keeps a tag it was given directly even if
/// it leaves the group. Answers with how many file assignments were added.
pub fn propagate_group_tags(conn: &mut dyn Db, shoot_id: Option<i64>) -> Result<usize> {
    let mut added = 0usize;
    for (kind, key, _value_id, tag, value) in group_assignments(conn, shoot_id)? {
        let Some(group_id) = resolve_group_key(conn, &kind, &key)? else { continue };
        let media: Vec<String> = group_media_ids(conn, &kind, group_id)?
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        added += assign_many(conn, "media", &media, &tag, &value)?;
    }
    Ok(added)
}


// --- smart collections: the taxonomy as a tree of media --------------------------

/// One tag = value pair, as a filter. `name` may be `None` to match the
/// value under any tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TagFilter {
    pub name: Option<String>,
    pub value: String,
}

/// One node of the smart tree: a tag value and how many files carry it
/// within the current selection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SmartNode {
    pub tag: String,
    pub value: String,
    pub media_count: i64,
}

/// `AND`-ed `EXISTS` clauses restricting media row alias `m` (with `m.id`)
/// to files carrying every filter. Parameters are appended to `args` in
/// order; `next` is the number of the first parameter to use.
pub fn media_filters_sql(filters: &[TagFilter], next: usize) -> (String, Vec<String>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    for (index, filter) in filters.iter().enumerate() {
        let tag_param = next + index * 2;
        let value_param = tag_param + 1;
        clauses.push(media_filter_sql(tag_param, value_param));
        params.push(filter.name.as_deref().map(clean).unwrap_or_default());
        params.push(clean(&filter.value));
    }
    (clauses.join(" AND "), params)
}

/// The tag values present on files matching `filters`, with counts —
/// restricted to one tag when `group_by` names it. Pairs already in
/// `filters` are left out, since every file has them.
pub fn smart_nodes(conn: &mut dyn Db, filters: &[TagFilter], group_by: Option<&str>) -> Result<Vec<SmartNode>> {
    let group_by = group_by.map(clean).unwrap_or_default();
    // $1 is the grouping tag; the filter parameters follow.
    let (where_filters, params) = media_filters_sql(filters, 2);
    let filter_clause = if where_filters.is_empty() { String::new() } else { format!(" AND {where_filters}") };
    let sql = format!(
        "SELECT t.name, v.value, COUNT(DISTINCT m.id) AS media_count
           FROM media m
           JOIN asset_tags a ON a.asset_kind = 'media' AND a.asset_key = m.id::text
           JOIN tag_values v ON v.id = a.tag_value_id
           JOIN tags t ON t.id = v.tag_id
          WHERE ($1 = '' OR t.name = $1){filter_clause}
          GROUP BY t.name, v.value
          ORDER BY t.name, media_count DESC, v.value"
    );
    let mut args: Vec<Box<dyn postgres::types::ToSql + Sync>> = vec![Box::new(group_by)];
    for param in params {
        args.push(Box::new(param));
    }
    let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = args.iter().map(|a| a.as_ref()).collect();
    let rows = conn.rows(&sql, &refs)?;
    let mut out = Vec::new();
    for row in &rows {
        let node = SmartNode {
            tag: get(row, "name")?,
            value: get(row, "value")?,
            media_count: get(row, "media_count")?,
        };
        let already = filters.iter().any(|f| {
            f.value.eq_ignore_ascii_case(&node.value)
                && f.name.as_deref().map_or(true, |n| n.eq_ignore_ascii_case(&node.tag))
        });
        if !already {
            out.push(node);
        }
    }
    Ok(out)
}

/// Tag values on the files of one manual group, with counts — what a
/// collection page shows as "tags in this collection".
pub fn tags_in_group(conn: &mut dyn Db, group_id: i64) -> Result<Vec<SmartNode>> {
    conn.rows(
        "SELECT t.name, v.value, COUNT(DISTINCT i.media_id) AS media_count
           FROM media_group_items i
           JOIN asset_tags a ON a.asset_kind = 'media' AND a.asset_key = i.media_id::text
           JOIN tag_values v ON v.id = a.tag_value_id
           JOIN tags t ON t.id = v.tag_id
          WHERE i.group_id = $1
          GROUP BY t.name, v.value
          ORDER BY t.name, media_count DESC, v.value",
        params![group_id],
    )?
    .iter()
    .map(|row| {
        Ok(SmartNode {
            tag: get(row, "name")?,
            value: get(row, "value")?,
            media_count: get(row, "media_count")?,
        })
    })
    .collect()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn tags_and_values_are_created_on_first_use_and_matched_without_case() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        assign(&mut conn, "media", "1", "Team", "Gods Reign").unwrap();
        assign(&mut conn, "media", "1", "team", "gods reign").unwrap();
        assign(&mut conn, "media", "1", "Team", "Velocity").unwrap();
        assign(&mut conn, "media", "2", "Venue", "Arena A").unwrap();

        let tags = list(&mut conn).unwrap();
        assert_eq!(tags.len(), 2, "{tags:?}");
        let team = tags.iter().find(|t| t.name == "Team").unwrap();
        assert_eq!(team.values.len(), 2, "case-insensitive value match: {:?}", team.values);
        assert_eq!(team.values.iter().find(|v| v.value == "Gods Reign").unwrap().uses, 1);

        let on_one = for_asset(&mut conn, "media", "1").unwrap();
        assert_eq!(on_one.len(), 2, "one tag, two values on one asset");
        assert!(on_one.iter().all(|t| t.tag == "Team"));

        let many = for_assets(&mut conn, "media", &["1".into(), "2".into(), "3".into()]).unwrap();
        assert_eq!(many["1"].len(), 2);
        assert_eq!(many["2"][0].tag, "Venue");
        assert!(!many.contains_key("3"));

        let value_id = on_one[0].value_id;
        assert!(unassign(&mut conn, "media", "1", value_id).unwrap());
        assert!(!unassign(&mut conn, "media", "1", value_id).unwrap());
        assert_eq!(for_asset(&mut conn, "media", "1").unwrap().len(), 1);
        let team = get_tag(&mut conn, team.id).unwrap().unwrap();
        assert_eq!(team.values.len(), 2, "the value survives in the taxonomy");

        assert!(assign(&mut conn, "shoot", "1", "Team", "X").is_err(), "unknown asset kind");
        assert!(assign(&mut conn, "media", "1", "  ", "X").is_err(), "blank tag name");
    }

    #[test]
    fn suggestions_come_from_recorded_values_most_used_first() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        for key in ["1", "2", "3"] {
            assign(&mut conn, "media", key, "Team", "Gods Reign").unwrap();
        }
        assign(&mut conn, "media", "1", "Team", "Godlike").unwrap();
        assign(&mut conn, "media", "1", "Venue", "Godavari Hall").unwrap();

        let within = suggest(&mut conn, Some("Team"), "god", 10).unwrap();
        assert_eq!(within.iter().map(|s| s.value.as_str()).collect::<Vec<_>>(), ["Gods Reign", "Godlike"]);
        let anywhere = suggest(&mut conn, None, "god", 10).unwrap();
        assert_eq!(anywhere.len(), 3);
        assert_eq!(anywhere[0].value, "Gods Reign");
        assert!(suggest(&mut conn, Some("Team"), "zzz", 10).unwrap().is_empty());
    }

    #[test]
    fn import_merges_and_reports_what_was_new() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let first = import(
            &mut conn,
            &[
                TaxonomyEntry { name: "Team".into(), values: vec!["A".into(), "B".into()] },
                TaxonomyEntry { name: "Venue".into(), values: vec!["Arena".into()] },
                TaxonomyEntry { name: "  ".into(), values: vec!["ignored".into()] },
            ],
        )
        .unwrap();
        assert_eq!((first.tags_created, first.values_created), (2, 3));
        let again = import(
            &mut conn,
            &[TaxonomyEntry { name: "team".into(), values: vec!["a".into(), "C".into(), "".into()] }],
        )
        .unwrap();
        assert_eq!((again.tags_created, again.values_created, again.values_seen), (0, 1, 2));
        let team = list(&mut conn).unwrap().into_iter().find(|t| t.name == "Team").unwrap();
        assert_eq!(team.values.len(), 3);

        rename_tag(&mut conn, team.id, "Squad").unwrap();
        assert!(delete_value(&mut conn, team.values[0].id).unwrap());
        assert!(delete_tag(&mut conn, team.id).unwrap());
        assert_eq!(list(&mut conn).unwrap().len(), 1);
    }

    #[test]
    fn group_tags_are_written_onto_the_files_in_the_group() {
        use crate::models::{MediaType, NewMedia};
        use crate::repo::{clusters, faces, media, shoots};
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:/s").unwrap();
        let mut ids = Vec::new();
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            ids.push(
                media::upsert(
                    &mut conn,
                    &NewMedia {
                        shoot_id: shoot.id,
                        path: format!("C:/s/{name}"),
                        filename: name.into(),
                        media_type: MediaType::Photo,
                        extension: "jpg".into(),
                        file_size: 1,
                        content_key: name.into(),
                        captured_at: None,
                        normalized_relative_path: None,
                    },
                )
                .unwrap(),
            );
        }
        // An album keyed by what it is, holding two files.
        let album_id: i64 = conn
            .row_one(
                "INSERT INTO albums (shoot_id, name, album_type, person_ids, generated_at) VALUES ($1, 'Team X', 'team', '[]', 'now') RETURNING id",
                params![shoot.id],
            )
            .unwrap()
            .get(0);
        for id in &ids[..2] {
            conn.exec("INSERT INTO album_media (album_id, media_id) VALUES ($1, $2)", params![album_id, id]).unwrap();
        }
        let key = format!("shoot:{}/team:Team X", shoot.id);
        assign(&mut conn, "album", &key, "Venue", "Arena").unwrap();
        assert_eq!(resolve_group_key(&mut conn, "album", &key).unwrap(), Some(album_id));
        assert!(resolve_group_key(&mut conn, "album", "shoot:1/team:Nope").unwrap().is_none());
        assert!(resolve_group_key(&mut conn, "album", "garbage").unwrap().is_none());

        // A cluster keyed by id, holding the third file through a face.
        let cluster_id = clusters::create(&mut conn, shoot.id, "Unknown 1").unwrap();
        let face_id = faces::insert(
            &mut conn,
            &crate::models::NewFace {
                media_id: ids[2],
                shoot_id: shoot.id,
                bbox: Default::default(),
                landmarks: None,
                detection_confidence: 0.9,
                embedding: None,
                quality: None,
                frame_time: None,
                crop_path: None,
                model_key: None,
            },
        )
        .unwrap();
        faces::set_cluster(&mut conn, face_id, Some(cluster_id)).unwrap();
        let ckey = format!("shoot:{}/cluster:{cluster_id}", shoot.id);
        assign(&mut conn, "cluster", &ckey, "Team", "Nebula").unwrap();

        let added = propagate_group_tags(&mut conn, Some(shoot.id)).unwrap();
        assert_eq!(added, 3);
        assert_eq!(media_ids_with_value(&mut conn, Some("Venue"), "Arena").unwrap(), ids[..2].to_vec());
        assert_eq!(media_ids_with_value(&mut conn, None, "Nebula").unwrap(), vec![ids[2]]);
        assert_eq!(propagate_group_tags(&mut conn, None).unwrap(), 0, "idempotent");
    }

    #[test]
    fn smart_nodes_count_files_under_each_value_within_the_selection() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        // Three files: two Nebula (one Final, one Semi), one Velocity (Final).
        assign(&mut conn, "media", "1", "Company", "Nebula").unwrap();
        assign(&mut conn, "media", "1", "Stage", "Final").unwrap();
        assign(&mut conn, "media", "2", "Company", "Nebula").unwrap();
        assign(&mut conn, "media", "2", "Stage", "Semi").unwrap();
        assign(&mut conn, "media", "3", "Company", "Velocity").unwrap();
        assign(&mut conn, "media", "3", "Stage", "Final").unwrap();
        // `media` rows must exist for the join: fake three.
        for id in 1i64..=3 {
            conn.exec(
                "INSERT INTO shoots (id, name, source_path, status, created_at, updated_at) VALUES (100, 'S', 'C:/s', 'completed', 'now', 'now') ON CONFLICT DO NOTHING",
                params![],
            )
            .unwrap();
            conn.exec(
                "INSERT INTO media (id, shoot_id, path, filename, media_type, extension, file_size, content_key, indexed_at, processing_status)
                 VALUES ($1, 100, $2, $2, 'photo', 'jpg', 1, $2, 'now', 'analysed') ON CONFLICT DO NOTHING",
                params![id, format!("f{id}.jpg")],
            )
            .unwrap();
        }
        let top = smart_nodes(&mut conn, &[], Some("Company")).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!((top[0].value.as_str(), top[0].media_count), ("Nebula", 2));
        assert_eq!((top[1].value.as_str(), top[1].media_count), ("Velocity", 1));

        let inside = smart_nodes(&mut conn, &[TagFilter { name: Some("Company".into()), value: "Nebula".into() }], None).unwrap();
        assert_eq!(
            inside.iter().map(|n| (n.tag.as_str(), n.value.as_str(), n.media_count)).collect::<Vec<_>>(),
            vec![("Stage", "Final", 1), ("Stage", "Semi", 1)],
            "the Company filter itself is not repeated"
        );
        let deeper = smart_nodes(
            &mut conn,
            &[
                TagFilter { name: Some("Company".into()), value: "Nebula".into() },
                TagFilter { name: Some("Stage".into()), value: "Final".into() },
            ],
            None,
        )
        .unwrap();
        assert!(deeper.is_empty(), "nothing left to split by: {deeper:?}");
    }
}
