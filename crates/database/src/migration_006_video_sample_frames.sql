-- Every successfully analysed video frame is a review surface, including
-- frames where automatic detection found no faces. The source video remains
-- the only media file; this table stores only lightweight timestamps.
CREATE TABLE video_sample_frames (
    media_id   INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    timestamp  REAL    NOT NULL,
    created_at TEXT    NOT NULL,
    PRIMARY KEY (media_id, timestamp)
);

CREATE INDEX idx_video_sample_frames_media
    ON video_sample_frames (media_id, timestamp);

-- Existing installations can immediately review every sample that produced a
-- face. A normal reanalysis fills in samples where detection found nobody.
INSERT OR IGNORE INTO video_sample_frames (media_id, timestamp, created_at)
SELECT media_id, frame_time, created_at
  FROM faces
 WHERE frame_time IS NOT NULL;

INSERT OR IGNORE INTO video_sample_frames (media_id, timestamp, created_at)
SELECT media_id, timestamp, datetime('now')
  FROM video_detections;
