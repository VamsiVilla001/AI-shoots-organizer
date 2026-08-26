-- Photo quality and perceptual-duplicate metadata (migration 3).
-- Scores are derived from cached thumbnails; originals remain read-only.

ALTER TABLE media ADD COLUMN quality_score REAL;
ALTER TABLE media ADD COLUMN sharpness_score REAL;
ALTER TABLE media ADD COLUMN exposure_score REAL;
ALTER TABLE media ADD COLUMN perceptual_hash TEXT;
ALTER TABLE media ADD COLUMN duplicate_group_id INTEGER;
ALTER TABLE media ADD COLUMN duplicate_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE media ADD COLUMN is_best_shot INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_media_quality ON media (shoot_id, media_type, quality_score DESC);
CREATE INDEX idx_media_duplicates ON media (shoot_id, duplicate_group_id);
