//! Durable project organisation and local-account access control.

use std::collections::HashSet;

use crate::client::Db;
use crate::models::{Project, ProjectCollection, ProjectCollectionSource, ProjectMember};
use crate::{now, params, DbError, Result};

fn clean_text(value: &str, maximum: usize, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(DbError::other(format!("give the {label} a name")));
    }
    if value.chars().count() > maximum {
        return Err(DbError::other(format!("the {label} is too long")));
    }
    Ok(value.to_string())
}

fn clean_visibility(value: &str) -> Result<&str> {
    match value {
        "private" | "invited" | "organisation" => Ok(value),
        _ => Err(DbError::other("choose a valid project visibility")),
    }
}

fn clean_status(value: &str) -> Result<&str> {
    match value {
        "active" | "archived" => Ok(value),
        _ => Err(DbError::other("choose a valid project status")),
    }
}

fn access_role(
    conn: &mut dyn Db,
    project_id: &str,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<Option<String>> {
    let row = conn.row_opt(
        "SELECT owner_account_id, visibility, organisation FROM projects WHERE id = $1",
        params![project_id],
    )?;
    let Some(row) = row else {
        return Ok(None);
    };
    let owner: String = super::at(&row, 0)?;
    let visibility: String = super::at(&row, 1)?;
    let project_organisation: Option<String> = super::at(&row, 2)?;

    if owner == account_id {
        return Ok(Some("owner".into()));
    }
    // `project_members.email` carries the `nocase` collation, so the
    // `COLLATE NOCASE` this used to spell out is no longer needed here.
    let member: Option<String> = conn
        .row_opt(
            "SELECT role FROM project_members WHERE project_id = $1 AND email = $2",
            params![project_id, email.trim()],
        )?
        .as_ref()
        .map(|row| super::at(row, 0))
        .transpose()?;
    let same_organisation = visibility == "organisation"
        && organisation.is_some_and(|current| {
            project_organisation
                .as_deref()
                .is_some_and(|project| !project.is_empty() && project.eq_ignore_ascii_case(current))
        });
    Ok(member.or_else(|| same_organisation.then(|| "viewer".into())))
}

fn require_editor(
    conn: &mut dyn Db,
    project_id: &str,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<String> {
    let role = access_role(conn, project_id, account_id, email, organisation)?
        .ok_or_else(|| DbError::other("you do not have access to this project"))?;
    if role == "viewer" {
        return Err(DbError::other("viewers cannot change this project"));
    }
    Ok(role)
}

fn require_owner(conn: &mut dyn Db, project_id: &str, account_id: &str) -> Result<()> {
    let owner: Option<String> = conn
        .row_opt(
            "SELECT owner_account_id FROM projects WHERE id = $1",
            params![project_id],
        )?
        .as_ref()
        .map(|row| super::at(row, 0))
        .transpose()?;
    match owner {
        Some(owner) if owner == account_id => Ok(()),
        Some(_) => Err(DbError::other("only the project owner can do that")),
        None => Err(DbError::other("that project no longer exists")),
    }
}

pub fn list_accessible(
    conn: &mut dyn Db,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<Vec<Project>> {
    // `projects.organisation` and `projects.name` are plain `TEXT`, so their
    // case-insensitive comparison and ordering name the collation inline;
    // `project_members.email` carries it on the column.
    //
    // `GROUP BY p.id` rather than `SELECT DISTINCT`: the member join can match
    // a project more than once, and both forms collapse that — but Postgres
    // rejects an `ORDER BY` expression that is not in a `DISTINCT` query's
    // select list, which `p.name COLLATE nocase` is not.
    let ids: Vec<String> = conn
        .rows(
            "SELECT p.id, p.updated_at, p.name
               FROM projects p
               LEFT JOIN project_members pm ON pm.project_id = p.id AND pm.email = $2
              WHERE p.owner_account_id = $1
                 OR (p.visibility = 'organisation' AND p.organisation = ($3 COLLATE nocase) AND $3 <> '')
                 OR pm.email IS NOT NULL
              GROUP BY p.id
              ORDER BY p.updated_at DESC, p.name COLLATE nocase",
            params![account_id, email.trim(), organisation.unwrap_or("")],
        )?
        .iter()
        .map(|row| super::at(row, 0))
        .collect::<Result<Vec<_>>>()?;

    ids.into_iter()
        .filter_map(|id| match get(conn, &id, account_id, email, organisation) {
            Ok(Some(project)) => Some(Ok(project)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub fn get(
    conn: &mut dyn Db,
    id: &str,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<Option<Project>> {
    let Some(role) = access_role(conn, id, account_id, email, organisation)? else {
        return Ok(None);
    };
    let base = conn.row_opt(
        "SELECT name, kind, owner_account_id, owner_email, organisation, visibility, status,
                cover_media_id, created_at, updated_at
           FROM projects WHERE id = $1",
        params![id],
    )?;
    let Some(base) = base else {
        return Ok(None);
    };
    let name: String = super::at(&base, 0)?;
    let kind: String = super::at(&base, 1)?;
    let owner_account_id: String = super::at(&base, 2)?;
    let owner_email: String = super::at(&base, 3)?;
    let project_organisation: Option<String> = super::at(&base, 4)?;
    let visibility: String = super::at(&base, 5)?;
    let status: String = super::at(&base, 6)?;
    let cover_media_id: Option<i64> = super::at(&base, 7)?;
    let created_at: String = super::at(&base, 8)?;
    let updated_at: String = super::at(&base, 9)?;

    #[allow(clippy::type_complexity)]
    let collection_rows: Vec<(String, Option<String>, String, Option<String>, i64, String, String)> = conn
        .rows(
            "SELECT id, parent_id, name, notes, sort_order, created_at, updated_at
               FROM project_collections WHERE project_id = $1
              ORDER BY sort_order, name",
            params![id],
        )?
        .iter()
        .map(|row| {
            Ok((
                super::at(row, 0)?,
                super::at(row, 1)?,
                super::at(row, 2)?,
                super::at(row, 3)?,
                super::at(row, 4)?,
                super::at(row, 5)?,
                super::at(row, 6)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut collections = Vec::with_capacity(collection_rows.len());
    for (collection_id, parent_id, collection_name, notes, sort_order, collection_created, collection_updated) in
        collection_rows
    {
        let sources = conn
            .rows(
                "SELECT shoot_id, group_id FROM project_collection_sources
                  WHERE collection_id = $1 ORDER BY added_at, group_id",
                params![collection_id],
            )?
            .iter()
            .map(|row| {
                Ok(ProjectCollectionSource {
                    shoot_id: super::at(row, 0)?,
                    group_id: super::at(row, 1)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        collections.push(ProjectCollection {
            id: collection_id,
            project_id: id.to_string(),
            parent_id,
            name: collection_name,
            notes,
            sort_order,
            sources,
            created_at: collection_created,
            updated_at: collection_updated,
        });
    }

    let members = conn
        .rows(
            "SELECT email, display_name, role, invitation_state FROM project_members
              WHERE project_id = $1 ORDER BY email",
            params![id],
        )?
        .iter()
        .map(|row| {
            Ok(ProjectMember {
                email: super::at(row, 0)?,
                display_name: super::at(row, 1)?,
                role: super::at(row, 2)?,
                invitation_state: super::at(row, 3)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let media_count: i64 = super::at(
        &conn.row_one(
            "SELECT COUNT(DISTINCT mgi.media_id)
               FROM project_collection_sources pcs
               JOIN project_collections pc ON pc.id = pcs.collection_id
               JOIN media_group_items mgi ON mgi.group_id = pcs.group_id
              WHERE pc.project_id = $1",
            params![id],
        )?,
        0,
    )?;

    Ok(Some(Project {
        id: id.to_string(),
        name,
        kind,
        owner_account_id,
        owner_email,
        organisation: project_organisation,
        visibility,
        status,
        cover_media_id,
        access_role: role,
        collections,
        members,
        media_count,
        created_at,
        updated_at,
    }))
}

/// Creates or updates a project and its collection tree. Membership is managed separately.
pub fn save(
    conn: &mut dyn Db,
    project: &Project,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<()> {
    let name = clean_text(&project.name, 120, "project")?;
    let kind = clean_text(&project.kind, 80, "project type")?;
    let visibility = clean_visibility(&project.visibility)?;
    let status = clean_status(&project.status)?;
    let existing = conn.row_opt(
        "SELECT owner_account_id, visibility, status, created_at FROM projects WHERE id = $1",
        params![project.id],
    )?;
    let existing = existing
        .as_ref()
        .map(|row| -> Result<(String, String, String, String)> {
            Ok((
                super::at(row, 0)?,
                super::at(row, 1)?,
                super::at(row, 2)?,
                super::at(row, 3)?,
            ))
        })
        .transpose()?;

    let stamp = now();
    let (owner, saved_visibility, saved_status, created_at) =
        if let Some((owner, old_visibility, old_status, created)) = existing {
            let role = require_editor(conn, &project.id, account_id, email, organisation)?;
            if role == "owner" {
                (owner, visibility.to_string(), status.to_string(), created)
            } else {
                (owner, old_visibility, old_status, created)
            }
        } else {
            (
                account_id.to_string(),
                visibility.to_string(),
                status.to_string(),
                stamp.clone(),
            )
        };
    let owner_email = if owner == account_id {
        email.trim().to_lowercase()
    } else {
        project.owner_email.clone()
    };
    let organisation_value = organisation.map(str::trim).filter(|value| !value.is_empty());
    conn.exec(
        "INSERT INTO projects (id, owner_account_id, owner_email, organisation, name, kind, visibility, status, cover_media_id, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, kind=excluded.kind,
             visibility=excluded.visibility, status=excluded.status, cover_media_id=excluded.cover_media_id,
             updated_at=excluded.updated_at",
        params![
            project.id,
            owner,
            owner_email,
            organisation_value,
            name,
            kind,
            saved_visibility,
            saved_status,
            project.cover_media_id,
            created_at,
            stamp
        ],
    )?;

    conn.exec(
        "DELETE FROM project_collections WHERE project_id = $1",
        params![project.id],
    )?;
    let mut pending = project.collections.clone();
    let ids: HashSet<_> = pending.iter().map(|item| item.id.as_str()).collect();
    if ids.len() != pending.len() {
        return Err(DbError::other("a project contains duplicate collection identifiers"));
    }
    let mut inserted = HashSet::<String>::new();
    while !pending.is_empty() {
        let before = pending.len();
        let mut index = 0;
        while index < pending.len() {
            let ready = pending[index]
                .parent_id
                .as_ref()
                .is_none_or(|parent| inserted.contains(parent));
            if !ready {
                index += 1;
                continue;
            }
            let collection = pending.remove(index);
            let collection_name = clean_text(&collection.name, 120, "collection")?;
            let collection_created = if collection.created_at.is_empty() {
                stamp.clone()
            } else {
                collection.created_at.clone()
            };
            let notes = collection
                .notes
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            conn.exec(
                "INSERT INTO project_collections (id, project_id, parent_id, name, notes, sort_order, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                params![
                    collection.id,
                    project.id,
                    collection.parent_id,
                    collection_name,
                    notes,
                    collection.sort_order,
                    collection_created,
                    stamp
                ],
            )?;
            for source in collection.sources {
                // The `g.shoot_id = $4` join is what stops a collection citing a
                // group from a different shoot; it simply selects no row.
                conn.exec(
                    "INSERT INTO project_collection_sources (collection_id, shoot_id, group_id, added_at)
                     SELECT $1, g.shoot_id, g.id, $3 FROM media_groups g
                      WHERE g.id = $2 AND g.shoot_id = $4
                     ON CONFLICT (collection_id, shoot_id, group_id) DO NOTHING",
                    params![collection.id, source.group_id, stamp, source.shoot_id],
                )?;
            }
            inserted.insert(collection.id);
        }
        if pending.len() == before {
            return Err(DbError::other("a collection parent is missing or creates a cycle"));
        }
    }
    Ok(())
}

pub fn delete(conn: &mut dyn Db, project_id: &str, account_id: &str) -> Result<()> {
    require_owner(conn, project_id, account_id)?;
    conn.exec("DELETE FROM projects WHERE id = $1", params![project_id])?;
    Ok(())
}

/// Replaces the whole member list in one transaction.
///
/// This takes the [`crate::Database`] rather than a connection because the
/// delete-then-reinsert must not be observable half-done, and rusqlite's
/// `unchecked_transaction` — which opened a transaction on a shared
/// `&Connection` — has no Postgres equivalent. Going through
/// [`crate::Database::transaction`] is the explicit version of what that did.
pub fn replace_members(
    db: &crate::Database,
    project_id: &str,
    members: &[ProjectMember],
    account_id: &str,
    owner_email: &str,
) -> Result<()> {
    db.transaction(|tx| replace_members_inner(tx, project_id, members, account_id, owner_email))
}

/// The body of [`replace_members`], for callers that already hold a transaction.
pub fn replace_members_inner(
    conn: &mut dyn Db,
    project_id: &str,
    members: &[ProjectMember],
    account_id: &str,
    owner_email: &str,
) -> Result<()> {
    require_owner(conn, project_id, account_id)?;
    conn.exec("DELETE FROM project_members WHERE project_id = $1", params![project_id])?;
    let stamp = now();
    let mut inserted_members = 0_i64;
    for member in members {
        let email = member.email.trim().to_lowercase();
        if email.is_empty() || !email.contains('@') {
            return Err(DbError::other("enter a valid member email address"));
        }
        if email.eq_ignore_ascii_case(owner_email) {
            continue;
        }
        let role = match member.role.as_str() {
            "editor" | "viewer" => member.role.as_str(),
            _ => return Err(DbError::other("choose a valid project role")),
        };
        let display_name = member
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        conn.exec(
            "INSERT INTO project_members (project_id, email, display_name, role, invitation_state, created_at)
             VALUES ($1, $2, $3, $4, 'invited', $5)",
            params![project_id, email, display_name, role, stamp],
        )?;
        inserted_members += 1;
    }
    // `$3::bigint` rather than `$3`: compared only against the bare literals
    // `0`, Postgres infers the parameter as `int4` and then rejects the `i64`
    // the caller sends. Nothing in the statement types it otherwise.
    conn.exec(
        "UPDATE projects
            SET visibility = CASE
                  WHEN $3::bigint > 0 AND visibility = 'private' THEN 'invited'
                  WHEN $3::bigint = 0 AND visibility = 'invited' THEN 'private'
                  ELSE visibility
                END,
                updated_at = $2
          WHERE id = $1",
        params![project_id, stamp, inserted_members],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{models::Project, Database};

    fn empty_project(id: &str, visibility: &str) -> Project {
        Project {
            id: id.into(),
            name: "BGIS 2026".into(),
            kind: "Esports tournament".into(),
            owner_account_id: String::new(),
            owner_email: String::new(),
            organisation: None,
            visibility: visibility.into(),
            status: "active".into(),
            cover_media_id: None,
            access_role: String::new(),
            collections: vec![],
            members: vec![],
            media_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn owners_and_organisation_viewers_see_the_expected_projects() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        save(
            &mut conn,
            &empty_project("private", "private"),
            "owner",
            "owner@example.com",
            Some("SKWAD"),
        )
        .unwrap();
        save(
            &mut conn,
            &empty_project("org", "organisation"),
            "owner",
            "owner@example.com",
            Some("SKWAD"),
        )
        .unwrap();
        assert_eq!(
            list_accessible(&mut conn, "owner", "owner@example.com", Some("SKWAD"))
                .unwrap()
                .len(),
            2
        );
        let visible = list_accessible(&mut conn, "other", "other@example.com", Some("SKWAD")).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].access_role, "viewer");
    }

    /// The organisation match is case-insensitive, which under SQLite came from
    /// `COLLATE NOCASE` on the comparison and here from the explicit
    /// `COLLATE nocase` on the parameter.
    #[test]
    fn organisation_visibility_ignores_case() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        save(
            &mut conn,
            &empty_project("org", "organisation"),
            "owner",
            "owner@example.com",
            Some("SKWAD"),
        )
        .unwrap();
        let visible = list_accessible(&mut conn, "other", "other@example.com", Some("skwad")).unwrap();
        assert_eq!(visible.len(), 1, "a differently-cased org name still matches");
    }

    #[test]
    fn invited_editors_can_update_collections_but_not_visibility() {
        let db = Database::open_test().unwrap();
        let mut conn = db.conn().unwrap();
        let project = empty_project("shared", "invited");
        save(&mut conn, &project, "owner", "owner@example.com", Some("SKWAD")).unwrap();
        drop(conn);

        replace_members(
            &db,
            "shared",
            &[ProjectMember {
                email: "editor@example.com".into(),
                display_name: None,
                role: "editor".into(),
                invitation_state: "invited".into(),
            }],
            "owner",
            "owner@example.com",
        )
        .unwrap();

        let mut conn = db.conn().unwrap();
        let mut shared = get(&mut conn, "shared", "editor", "editor@example.com", Some("SKWAD"))
            .unwrap()
            .unwrap();
        shared.visibility = "organisation".into();
        shared.collections.push(ProjectCollection {
            id: "highlights".into(),
            project_id: "shared".into(),
            parent_id: None,
            name: "Highlights".into(),
            notes: None,
            sort_order: 0,
            sources: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        });
        save(&mut conn, &shared, "editor", "editor@example.com", Some("SKWAD")).unwrap();
        let updated = get(&mut conn, "shared", "owner", "owner@example.com", Some("SKWAD"))
            .unwrap()
            .unwrap();
        assert_eq!(updated.visibility, "invited");
        assert_eq!(updated.collections.len(), 1);
    }
}
