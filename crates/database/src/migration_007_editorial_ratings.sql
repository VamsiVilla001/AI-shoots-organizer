-- Human editorial decisions. These are intentionally stored on the media row
-- so re-analysis can rebuild derived AI data without losing an editor's work.
ALTER TABLE media ADD COLUMN rating INTEGER NOT NULL DEFAULT 0
    CHECK (rating BETWEEN 0 AND 5);

ALTER TABLE media ADD COLUMN pick_state TEXT NOT NULL DEFAULT 'none'
    CHECK (pick_state IN ('none', 'pick', 'reject'));

CREATE INDEX idx_media_editorial
    ON media (shoot_id, pick_state, rating DESC);
