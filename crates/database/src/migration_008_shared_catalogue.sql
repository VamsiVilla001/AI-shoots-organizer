-- Stable identities and offline-first collaboration scaffolding (V2.0).
-- Device-local NAS roots are kept in `library_mappings`; they are never copied
-- into a published catalogue or cloud metadata.

ALTER TABLE shoots ADD COLUMN stable_id TEXT;
ALTER TABLE shoots ADD COLUMN library_id TEXT;
ALTER TABLE shoots ADD COLUMN cloud_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shoots ADD COLUMN tombstone INTEGER NOT NULL DEFAULT 0;

ALTER TABLE people ADD COLUMN stable_id TEXT;
ALTER TABLE people ADD COLUMN cloud_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE people ADD COLUMN tombstone INTEGER NOT NULL DEFAULT 0;

ALTER TABLE media ADD COLUMN stable_id TEXT;
ALTER TABLE media ADD COLUMN normalized_relative_path TEXT;
ALTER TABLE media ADD COLUMN fps REAL;
ALTER TABLE media ADD COLUMN bitrate INTEGER;
ALTER TABLE media ADD COLUMN video_codec TEXT;
ALTER TABLE media ADD COLUMN audio_codec TEXT;
ALTER TABLE media ADD COLUMN cloud_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE media ADD COLUMN tombstone INTEGER NOT NULL DEFAULT 0;

ALTER TABLE clusters ADD COLUMN stable_id TEXT;
ALTER TABLE albums ADD COLUMN stable_id TEXT;
ALTER TABLE media_groups ADD COLUMN stable_id TEXT;

UPDATE shoots SET
    stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))),
    library_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)));
UPDATE people SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)));
UPDATE media SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)));
UPDATE clusters SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)));
UPDATE albums SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)));
UPDATE media_groups SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)));

CREATE UNIQUE INDEX idx_shoots_stable_id ON shoots (stable_id);
CREATE UNIQUE INDEX idx_people_stable_id ON people (stable_id);
CREATE UNIQUE INDEX idx_media_stable_id ON media (stable_id);
CREATE UNIQUE INDEX idx_clusters_stable_id ON clusters (stable_id);
CREATE UNIQUE INDEX idx_albums_stable_id ON albums (stable_id);
CREATE UNIQUE INDEX idx_media_groups_stable_id ON media_groups (stable_id);

CREATE TRIGGER shoots_assign_stable_ids AFTER INSERT ON shoots
WHEN NEW.stable_id IS NULL OR NEW.library_id IS NULL
BEGIN
    UPDATE shoots SET
        stable_id = coalesce(NEW.stable_id, lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)))),
        library_id = coalesce(NEW.library_id, lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))))
    WHERE id = NEW.id;
END;
CREATE TRIGGER people_assign_stable_id AFTER INSERT ON people WHEN NEW.stable_id IS NULL BEGIN UPDATE people SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))) WHERE id = NEW.id; END;
CREATE TRIGGER media_assign_stable_id AFTER INSERT ON media WHEN NEW.stable_id IS NULL BEGIN UPDATE media SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))) WHERE id = NEW.id; END;
CREATE TRIGGER clusters_assign_stable_id AFTER INSERT ON clusters WHEN NEW.stable_id IS NULL BEGIN UPDATE clusters SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))) WHERE id = NEW.id; END;
CREATE TRIGGER albums_assign_stable_id AFTER INSERT ON albums WHEN NEW.stable_id IS NULL BEGIN UPDATE albums SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))) WHERE id = NEW.id; END;
CREATE TRIGGER media_groups_assign_stable_id AFTER INSERT ON media_groups WHEN NEW.stable_id IS NULL BEGIN UPDATE media_groups SET stable_id = lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6))) WHERE id = NEW.id; END;

CREATE TABLE library_mappings (
    library_id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    local_root TEXT NOT NULL,
    approved_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE sync_outbox (
    id INTEGER PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    workspace_id TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    base_revision INTEGER NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    attempted_at TEXT,
    sent_at TEXT,
    error TEXT
);
CREATE INDEX idx_sync_outbox_pending ON sync_outbox (sent_at, id);

CREATE TABLE sync_conflicts (
    id INTEGER PRIMARY KEY,
    conflict_id TEXT NOT NULL UNIQUE,
    workspace_id TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    local_payload TEXT NOT NULL,
    remote_payload TEXT NOT NULL,
    detected_at TEXT NOT NULL,
    resolved_at TEXT,
    resolution TEXT
);

CREATE TABLE catalogue_revisions (
    revision_id TEXT PRIMARY KEY,
    package_id TEXT NOT NULL,
    shoot_id INTEGER REFERENCES shoots (id) ON DELETE SET NULL,
    revision_number INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('draft', 'published', 'applied', 'rolled_back')),
    object_key TEXT,
    manifest_hash TEXT,
    created_at TEXT NOT NULL,
    published_at TEXT,
    applied_at TEXT,
    UNIQUE (package_id, revision_number)
);

CREATE TABLE imported_catalogues (
    package_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    library_id TEXT NOT NULL,
    shoot_id TEXT NOT NULL,
    catalogue_hash TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    PRIMARY KEY (package_id, revision_id)
);
