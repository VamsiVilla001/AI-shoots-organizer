-- Preserve reviewer-drawn face boxes when a photo is analysed again.
ALTER TABLE faces ADD COLUMN source TEXT NOT NULL DEFAULT 'detected';

CREATE INDEX idx_faces_media_source ON faces (media_id, source);
