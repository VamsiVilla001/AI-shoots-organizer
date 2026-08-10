-- Migration 2 — group media by how many people are in it.
--
-- `face_count` cannot answer this. It counts face *rows*, and video analysis
-- inserts one row per detection per sampled frame, so a one-person interview
-- sampled across 20 frames has face_count = 20. `person_count` is the count of
-- distinct *people*, which is what the group-size albums need.

ALTER TABLE media ADD COLUMN person_count INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_media_person_count ON media (shoot_id, person_count);

-- Backfill, so databases created before this migration report real numbers
-- immediately instead of showing every file as "No people" until it is
-- re-analysed. Identical to repo::media::refresh_person_counts, minus the
-- per-shoot filter — see that function for what each term is for.
WITH per_frame AS (
    SELECT media_id, frame_time,
           COUNT(*) AS total,
           SUM(CASE WHEN person_id IS NULL AND cluster_id IS NULL THEN 1 ELSE 0 END) AS unknown
      FROM faces
     WHERE assignment != 'ignored'
     GROUP BY media_id, frame_time
),
frame_max AS (
    SELECT media_id, MAX(total) AS max_total, MAX(unknown) AS max_unknown
      FROM per_frame GROUP BY media_id
),
identified AS (
    SELECT media_id,
           COUNT(DISTINCT CASE WHEN person_id  IS NOT NULL THEN 'p' || person_id
                               WHEN cluster_id IS NOT NULL THEN 'c' || cluster_id END) AS c
      FROM faces
     WHERE assignment != 'ignored'
     GROUP BY media_id
)
UPDATE media SET person_count = MAX(
      COALESCE((SELECT c           FROM identified WHERE media_id = media.id), 0)
    + COALESCE((SELECT max_unknown FROM frame_max  WHERE media_id = media.id), 0),
      COALESCE((SELECT max_total   FROM frame_max  WHERE media_id = media.id), 0)
);
