//! Durable project organisation and local-account access control.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{Project, ProjectCollection, ProjectCollectionSource, ProjectMember};
use crate::{now, DbError, Result};

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
    conn: &Connection,
    project_id: &str,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<Option<String>> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT owner_account_id, visibility, organisation FROM projects WHERE id = ?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((owner, visibility, project_organisation)) = row else {
        return Ok(None);
    };
    if owner == account_id {
        return Ok(Some("owner".into()));
    }
    let member = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND email = ?2 COLLATE NOCASE",
            params![project_id, email.trim()],
            |row| row.get(0),
        )
        .optional()?;
    let same_organisation = visibility == "organisation"
        && organisation.is_some_and(|current| {
            project_organisation
                .as_deref()
                .is_some_and(|project| !project.is_empty() && project.eq_ignore_ascii_case(current))
        });
    Ok(member.or_else(|| same_organisation.then(|| "viewer".into())))
}

fn require_editor(
    conn: &Connection,
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

fn require_owner(conn: &Connection, project_id: &str, account_id: &str) -> Result<()> {
    let owner: Option<String> = conn
        .query_row(
            "SELECT owner_account_id FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?;
    match owner {
        Some(owner) if owner == account_id => Ok(()),
        Some(_) => Err(DbError::other("only the project owner can do that")),
        None => Err(DbError::other("that project no longer exists")),
    }
}

pub fn list_accessible(
    conn: &Connection,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<Vec<Project>> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT p.id
           FROM projects p
           LEFT JOIN project_members pm ON pm.project_id = p.id AND pm.email = ?2 COLLATE NOCASE
          WHERE p.owner_account_id = ?1
             OR (p.visibility = 'organisation' AND p.organisation = ?3 COLLATE NOCASE AND ?3 <> '')
             OR pm.email IS NOT NULL
          ORDER BY p.updated_at DESC, p.name COLLATE NOCASE",
    )?;
    let ids = statement
        .query_map(params![account_id, email.trim(), organisation.unwrap_or("")], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .filter_map(|id| match get(conn, &id, account_id, email, organisation) {
            Ok(Some(project)) => Some(Ok(project)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub fn get(
    conn: &Connection,
    id: &str,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<Option<Project>> {
    let Some(role) = access_role(conn, id, account_id, email, organisation)? else {
        return Ok(None);
    };
    let base = conn
        .query_row(
            "SELECT name, kind, owner_account_id, owner_email, organisation, visibility, status,
                    cover_media_id, created_at, updated_at
               FROM projects WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        name,
        kind,
        owner_account_id,
        owner_email,
        project_organisation,
        visibility,
        status,
        cover_media_id,
        created_at,
        updated_at,
    )) = base
    else {
        return Ok(None);
    };

    let mut collections_statement = conn.prepare(
        "SELECT id, parent_id, name, notes, sort_order, created_at, updated_at
           FROM project_collections WHERE project_id = ?1
          ORDER BY sort_order, name COLLATE NOCASE",
    )?;
    let collection_rows = collections_statement
        .query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut collections = Vec::with_capacity(collection_rows.len());
    for (collection_id, parent_id, collection_name, notes, sort_order, collection_created, collection_updated) in
        collection_rows
    {
        let mut source_statement = conn.prepare(
            "SELECT shoot_id, group_id FROM project_collection_sources
              WHERE collection_id = ?1 ORDER BY added_at, group_id",
        )?;
        let sources = source_statement
            .query_map([&collection_id], |row| {
                Ok(ProjectCollectionSource {
                    shoot_id: row.get(0)?,
                    group_id: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
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

    let mut member_statement = conn.prepare(
        "SELECT email, display_name, role, invitation_state FROM project_members
          WHERE project_id = ?1 ORDER BY email COLLATE NOCASE",
    )?;
    let members = member_statement
        .query_map([id], |row| {
            Ok(ProjectMember {
                email: row.get(0)?,
                display_name: row.get(1)?,
                role: row.get(2)?,
                invitation_state: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let media_count = conn.query_row(
        "SELECT COUNT(DISTINCT mgi.media_id)
           FROM project_collection_sources pcs
           JOIN project_collections pc ON pc.id = pcs.collection_id
           JOIN media_group_items mgi ON mgi.group_id = pcs.group_id
          WHERE pc.project_id = ?1",
        [id],
        |row| row.get(0),
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
    conn: &Connection,
    project: &Project,
    account_id: &str,
    email: &str,
    organisation: Option<&str>,
) -> Result<()> {
    let name = clean_text(&project.name, 120, "project")?;
    let kind = clean_text(&project.kind, 80, "project type")?;
    let visibility = clean_visibility(&project.visibility)?;
    let status = clean_status(&project.status)?;
    let existing = conn
        .query_row(
            "SELECT owner_account_id, visibility, status, created_at FROM projects WHERE id = ?1",
            [&project.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
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
    conn.execute(
        "INSERT INTO projects (id, owner_account_id, owner_email, organisation, name, kind, visibility, status, cover_media_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, kind=excluded.kind,
             visibility=excluded.visibility, status=excluded.status, cover_media_id=excluded.cover_media_id,
             updated_at=excluded.updated_at",
        params![project.id, owner, owner_email, organisation.map(str::trim).filter(|value| !value.is_empty()), name, kind, saved_visibility, saved_status, project.cover_media_id, created_at, stamp],
    )?;

    conn.execute("DELETE FROM project_collections WHERE project_id = ?1", [&project.id])?;
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
            conn.execute(
                "INSERT INTO project_collections (id, project_id, parent_id, name, notes, sort_order, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![collection.id, project.id, collection.parent_id, collection_name,
                    collection.notes.as_deref().map(str::trim).filter(|value| !value.is_empty()),
                    collection.sort_order, collection_created, stamp],
            )?;
            for source in collection.sources {
                conn.execute(
                    "INSERT OR IGNORE INTO project_collection_sources (collection_id, shoot_id, group_id, added_at)
                     SELECT ?1, g.shoot_id, g.id, ?3 FROM media_groups g
                      WHERE g.id = ?2 AND g.shoot_id = ?4",
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

pub fn delete(conn: &Connection, project_id: &str, account_id: &str) -> Result<()> {
    require_owner(conn, project_id, account_id)?;
    conn.execute("DELETE FROM projects WHERE id = ?1", [project_id])?;
    Ok(())
}

pub fn replace_members(
    conn: &Connection,
    project_id: &str,
    members: &[ProjectMember],
    account_id: &str,
    owner_email: &str,
) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    replace_members_inner(&transaction, project_id, members, account_id, owner_email)?;
    transaction.commit()?;
    Ok(())
}

fn replace_members_inner(
    conn: &Connection,
    project_id: &str,
    members: &[ProjectMember],
    account_id: &str,
    owner_email: &str,
) -> Result<()> {
    require_owner(conn, project_id, account_id)?;
    conn.execute("DELETE FROM project_members WHERE project_id = ?1", [project_id])?;
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
        conn.execute(
            "INSERT INTO project_members (project_id, email, display_name, role, invitation_state, created_at)
             VALUES (?1, ?2, ?3, ?4, 'invited', ?5)",
            params![
                project_id,
                email,
                member
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
                role,
                stamp
            ],
        )?;
        inserted_members += 1;
    }
    conn.execute(
        "UPDATE projects
            SET visibility = CASE
                  WHEN ?3 > 0 AND visibility = 'private' THEN 'invited'
                  WHEN ?3 = 0 AND visibility = 'invited' THEN 'private'
                  ELSE visibility
                END,
                updated_at = ?2
          WHERE id = ?1",
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
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        save(
            &conn,
            &empty_project("private", "private"),
            "owner",
            "owner@example.com",
            Some("SKWAD"),
        )
        .unwrap();
        save(
            &conn,
            &empty_project("org", "organisation"),
            "owner",
            "owner@example.com",
            Some("SKWAD"),
        )
        .unwrap();
        assert_eq!(
            list_accessible(&conn, "owner", "owner@example.com", Some("SKWAD"))
                .unwrap()
                .len(),
            2
        );
        let visible = list_accessible(&conn, "other", "other@example.com", Some("SKWAD")).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].access_role, "viewer");
    }

    #[test]
    fn invited_editors_can_update_collections_but_not_visibility() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let project = empty_project("shared", "invited");
        save(&conn, &project, "owner", "owner@example.com", Some("SKWAD")).unwrap();
        replace_members(
            &conn,
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
        let mut shared = get(&conn, "shared", "editor", "editor@example.com", Some("SKWAD"))
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
        save(&conn, &shared, "editor", "editor@example.com", Some("SKWAD")).unwrap();
        let updated = get(&conn, "shared", "owner", "owner@example.com", Some("SKWAD"))
            .unwrap()
            .unwrap();
        assert_eq!(updated.visibility, "invited");
        assert_eq!(updated.collections.len(), 1);
    }
}
