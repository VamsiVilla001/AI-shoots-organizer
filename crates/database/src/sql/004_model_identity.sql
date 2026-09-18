-- Model identity for embeddings.
--
-- Two machines can each hold a file that matches the "arcface" filename hint
-- and yet be different weights, producing vectors in different spaces. Compared
-- by cosine similarity they do not error; recognition just quietly degrades.
-- Every embedding therefore records the content hash of the embedder that
-- produced it. Rows from before this column are the NULL cohort — no backfill,
-- because there is no way to know what produced them.

ALTER TABLE faces ADD COLUMN model_key TEXT;

CREATE INDEX idx_faces_model ON faces (model_key);
