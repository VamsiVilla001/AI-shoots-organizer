-- Durable project organisation sits above imports and manual media groups.
-- Source files remain owned by their shoot; projects only store references.
CREATE TABLE projects (
    id                TEXT PRIMARY KEY,
    owner_account_id  TEXT NOT NULL,
    owner_email       TEXT NOT NULL,
    organisation     TEXT,
    name              TEXT NOT NULL,
    kind              TEXT NOT NULL,
    visibility        TEXT NOT NULL DEFAULT 'private'
                      CHECK (visibility IN ('private', 'invited', 'organisation')),
    status            TEXT NOT NULL DEFAULT 'active'
                      CHECK (status IN ('active', 'archived')),
    cover_media_id    INTEGER REFERENCES media(id) ON DELETE SET NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE INDEX idx_projects_owner ON projects(owner_account_id, status, updated_at DESC);
CREATE INDEX idx_projects_visibility ON projects(visibility, status, updated_at DESC);

CREATE TABLE project_members (
    project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    email             TEXT NOT NULL COLLATE NOCASE,
    display_name      TEXT,
    role              TEXT NOT NULL CHECK (role IN ('editor', 'viewer')),
    invitation_state  TEXT NOT NULL DEFAULT 'invited'
                      CHECK (invitation_state IN ('invited', 'accepted')),
    created_at        TEXT NOT NULL,
    PRIMARY KEY (project_id, email)
);

CREATE INDEX idx_project_members_email ON project_members(email COLLATE NOCASE);

CREATE TABLE project_collections (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    parent_id   TEXT REFERENCES project_collections(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    notes       TEXT,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX idx_project_collections_parent
    ON project_collections(project_id, parent_id, sort_order, name COLLATE NOCASE);
CREATE UNIQUE INDEX idx_project_collections_name
    ON project_collections(project_id, COALESCE(parent_id, ''), name COLLATE NOCASE);

CREATE TABLE project_collection_sources (
    collection_id  TEXT NOT NULL REFERENCES project_collections(id) ON DELETE CASCADE,
    shoot_id       INTEGER NOT NULL REFERENCES shoots(id) ON DELETE CASCADE,
    group_id       INTEGER NOT NULL REFERENCES media_groups(id) ON DELETE CASCADE,
    added_at       TEXT NOT NULL,
    PRIMARY KEY (collection_id, shoot_id, group_id)
);
