-- Path portability: the stored `media.path` is what the *scanner* saw, and it
-- is baked into `content_key`, so it can never be rewritten. Clients on other
-- machines instead resolve a file as
--
--     shoots.share_path / media.normalized_relative_path
--
-- Both halves are structural rather than a global `local_root -> unc_root`
-- string mapping: different shoots can live on different shares, and a shoot
-- with no `share_path` is simply server-only.

ALTER TABLE shoots ADD COLUMN share_path TEXT;

-- `normalized_relative_path` was declared in the baseline and exported by the
-- portable catalogue, but nothing ever wrote it. Backfill what can be derived:
-- rows whose path sits under the shoot's source root. Separators are folded to
-- `/`, which is the form the catalogue crate's `normalize_relative_path`
-- produces, so old and new rows agree.
UPDATE media m
   SET normalized_relative_path = replace(
           ltrim(substr(m.path, length(s.source_path) + 1), '\/'),
           '\', '/')
  FROM shoots s
 WHERE s.id = m.shoot_id
   AND m.normalized_relative_path IS NULL
   AND s.source_path <> ''
   AND left(m.path, length(s.source_path)) = s.source_path
   AND length(m.path) > length(s.source_path);
