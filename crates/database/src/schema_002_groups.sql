-- Manual grouping (migration 2)
--
-- Albums are *derived* from face assignments and get dropped and rebuilt on
-- every regenerate. An editor's own sorting decisions cannot live there, so
-- they get their own tables:
--
--   * `media_groups`      — a folder the editor named in the app. The name is
--                           the folder name on export; `folder_name` only
--                           exists for the case where a nicer on-disk name is
--                           wanted than the label in the UI.
--   * `media_group_items` — membership. One file may sit in several groups
--                           (the same clip can belong to two players), and a
--                           group never owns the file: deleting a row here
--                           deletes nothing on disk.
--
-- Counts are denormalised the same way `albums` does it, because the sorting
-- screen renders them on every keystroke.

CREATE TABLE media_groups (
    id             INTEGER PRIMARY KEY,
    shoot_id       INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    name           TEXT    NOT NULL COLLATE NOCASE,
    -- NULL means "use `name`"; set when the editor wants the folder on disk to
    -- differ from the label they work with.
    folder_name    TEXT,
    notes          TEXT,
    -- Set when the group was seeded from an AI player album, so the UI can say
    -- where it came from. Clearing the player never removes the group.
    person_id      INTEGER REFERENCES people (id) ON DELETE SET NULL,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    media_count    INTEGER NOT NULL DEFAULT 0,
    photo_count    INTEGER NOT NULL DEFAULT 0,
    video_count    INTEGER NOT NULL DEFAULT 0,
    cover_media_id INTEGER REFERENCES media (id) ON DELETE SET NULL,
    created_at     TEXT    NOT NULL,
    updated_at     TEXT    NOT NULL,
    UNIQUE (shoot_id, name)
);

CREATE INDEX idx_media_groups_shoot ON media_groups (shoot_id, sort_order, name COLLATE NOCASE);

CREATE TABLE media_group_items (
    group_id INTEGER NOT NULL REFERENCES media_groups (id) ON DELETE CASCADE,
    media_id INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    added_at TEXT    NOT NULL,
    PRIMARY KEY (group_id, media_id)
);

CREATE INDEX idx_media_group_items_media ON media_group_items (media_id);
