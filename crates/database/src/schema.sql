-- SKWAD Media Organiser — baseline schema (migration 1)
--
-- Design notes:
--   * Source media is never written to. Everything here is an *index* of files
--     that live in the user's own folders; deleting a row never deletes a file.
--   * `faces.embedding` holds a raw little-endian f32 vector. For the library
--     sizes this product targets (thousands of faces per shoot) a linear cosine
--     scan with rayon is fast enough; a vector index can be layered on later
--     without changing this table.

CREATE TABLE shoots (
    id           INTEGER PRIMARY KEY,
    name         TEXT    NOT NULL,
    source_path  TEXT    NOT NULL,
    status       TEXT    NOT NULL DEFAULT 'created',
    notes        TEXT,
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL
);

CREATE INDEX idx_shoots_created ON shoots (created_at DESC);

CREATE TABLE people (
    id            INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL COLLATE NOCASE,
    team          TEXT,
    notes         TEXT,
    cover_face_id INTEGER,
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL
);

CREATE UNIQUE INDEX idx_people_name ON people (name COLLATE NOCASE);
CREATE INDEX idx_people_team ON people (team);

CREATE TABLE media (
    id                INTEGER PRIMARY KEY,
    shoot_id          INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    path              TEXT    NOT NULL,
    filename          TEXT    NOT NULL,
    media_type        TEXT    NOT NULL,          -- photo | video
    extension         TEXT    NOT NULL,
    width             INTEGER,
    height            INTEGER,
    duration          REAL,                      -- seconds, videos only
    file_size         INTEGER NOT NULL DEFAULT 0,
    content_key       TEXT    NOT NULL,          -- blake3(path|size|mtime), drives thumbnail reuse
    captured_at       TEXT,                      -- EXIF capture time, else file mtime
    indexed_at        TEXT    NOT NULL,
    camera_make       TEXT,
    camera_model      TEXT,
    lens              TEXT,
    iso               INTEGER,
    focal_length      REAL,
    aperture          REAL,
    shutter           TEXT,
    orientation       INTEGER NOT NULL DEFAULT 1,
    thumbnail_path    TEXT,
    processing_status TEXT    NOT NULL DEFAULT 'pending',
    face_count        INTEGER NOT NULL DEFAULT 0,
    error             TEXT,
    UNIQUE (shoot_id, path)
);

CREATE INDEX idx_media_shoot ON media (shoot_id, media_type);
CREATE INDEX idx_media_status ON media (shoot_id, processing_status);
CREATE INDEX idx_media_captured ON media (shoot_id, captured_at);

CREATE TABLE clusters (
    id            INTEGER PRIMARY KEY,
    shoot_id      INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    label         TEXT    NOT NULL,              -- "Unknown Person 1"
    person_id     INTEGER REFERENCES people (id) ON DELETE SET NULL,
    status        TEXT    NOT NULL DEFAULT 'unnamed', -- unnamed | named | ignored
    face_count    INTEGER NOT NULL DEFAULT 0,
    cover_face_id INTEGER,
    created_at    TEXT    NOT NULL
);

CREATE INDEX idx_clusters_shoot ON clusters (shoot_id, status);

CREATE TABLE faces (
    id                     INTEGER PRIMARY KEY,
    media_id               INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    shoot_id               INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    person_id              INTEGER REFERENCES people (id) ON DELETE SET NULL,
    cluster_id             INTEGER REFERENCES clusters (id) ON DELETE SET NULL,
    embedding              BLOB,                 -- little-endian f32[embedding_dim]
    embedding_dim          INTEGER,
    bbox_x                 REAL    NOT NULL,     -- normalised 0..1 against the full frame
    bbox_y                 REAL    NOT NULL,
    bbox_w                 REAL    NOT NULL,
    bbox_h                 REAL    NOT NULL,
    landmarks              BLOB,                 -- little-endian f32[10] (5 points, pixel space)
    detection_confidence   REAL    NOT NULL,
    recognition_confidence REAL,
    assignment             TEXT    NOT NULL DEFAULT 'unassigned',
                                                 -- unassigned | suggested | confirmed | rejected | ignored
    quality                REAL,
    frame_time             REAL,                 -- seconds into the video the frame came from
    crop_path              TEXT,
    created_at             TEXT    NOT NULL
);

CREATE INDEX idx_faces_media ON faces (media_id);
CREATE INDEX idx_faces_person ON faces (person_id);
CREATE INDEX idx_faces_cluster ON faces (cluster_id);
CREATE INDEX idx_faces_shoot_assignment ON faces (shoot_id, assignment);

CREATE TABLE video_detections (
    id             INTEGER PRIMARY KEY,
    media_id       INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    person_id      INTEGER REFERENCES people (id) ON DELETE CASCADE,
    face_id        INTEGER REFERENCES faces (id) ON DELETE CASCADE,
    timestamp      REAL    NOT NULL,
    end_timestamp  REAL,
    confidence     REAL    NOT NULL
);

CREATE INDEX idx_video_detections_media ON video_detections (media_id, timestamp);
CREATE INDEX idx_video_detections_person ON video_detections (person_id);

CREATE TABLE albums (
    id             INTEGER PRIMARY KEY,
    shoot_id       INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    name           TEXT    NOT NULL,
    album_type     TEXT    NOT NULL,             -- player | multi_player | unidentified | team
    person_ids     TEXT,                         -- JSON array of person ids
    cluster_id     INTEGER REFERENCES clusters (id) ON DELETE CASCADE,
    cover_media_id INTEGER REFERENCES media (id) ON DELETE SET NULL,
    media_count    INTEGER NOT NULL DEFAULT 0,
    photo_count    INTEGER NOT NULL DEFAULT 0,
    video_count    INTEGER NOT NULL DEFAULT 0,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    generated_at   TEXT    NOT NULL,
    UNIQUE (shoot_id, album_type, name)
);

CREATE INDEX idx_albums_shoot ON albums (shoot_id, album_type, sort_order);

CREATE TABLE album_media (
    album_id INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    media_id INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    PRIMARY KEY (album_id, media_id)
);

CREATE INDEX idx_album_media_media ON album_media (media_id);

CREATE TABLE jobs (
    id          INTEGER PRIMARY KEY,
    shoot_id    INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    media_id    INTEGER REFERENCES media (id) ON DELETE CASCADE,
    kind        TEXT    NOT NULL,                -- scan | thumbnail | analyse_photo | analyse_video | recognise | cluster | albums
    state       TEXT    NOT NULL DEFAULT 'queued', -- queued | running | done | failed | cancelled
    priority    INTEGER NOT NULL DEFAULT 100,
    attempts    INTEGER NOT NULL DEFAULT 0,
    payload     TEXT,
    error       TEXT,
    created_at  TEXT    NOT NULL,
    started_at  TEXT,
    finished_at TEXT
);

CREATE INDEX idx_jobs_pick ON jobs (state, priority, id);
CREATE INDEX idx_jobs_shoot ON jobs (shoot_id, state);

CREATE TABLE exports (
    id          INTEGER PRIMARY KEY,
    shoot_id    INTEGER NOT NULL REFERENCES shoots (id) ON DELETE CASCADE,
    destination TEXT    NOT NULL,
    options     TEXT    NOT NULL,                -- JSON ExportOptions
    status      TEXT    NOT NULL DEFAULT 'queued',
    files_total INTEGER NOT NULL DEFAULT 0,
    files_done  INTEGER NOT NULL DEFAULT 0,
    bytes_done  INTEGER NOT NULL DEFAULT 0,
    error       TEXT,
    started_at  TEXT,
    finished_at TEXT
);

CREATE INDEX idx_exports_shoot ON exports (shoot_id, started_at DESC);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Deliberately stores identifiers and outcomes only; never embeddings or crops.
CREATE TABLE app_log (
    id        INTEGER PRIMARY KEY,
    timestamp TEXT NOT NULL,
    event     TEXT NOT NULL,
    shoot_id  INTEGER,
    media_id  INTEGER,
    person_id INTEGER,
    detail    TEXT
);

CREATE INDEX idx_app_log_time ON app_log (timestamp DESC);
CREATE INDEX idx_app_log_event ON app_log (event, timestamp DESC);
