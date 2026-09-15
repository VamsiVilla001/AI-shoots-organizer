-- Person enrollment ("Pre-Process"): reference photos/video are parked in one
-- hidden system shoot rather than loosening the NOT NULL shoot_id on faces,
-- media and jobs everywhere those are queried.
ALTER TABLE shoots ADD COLUMN is_reference INTEGER NOT NULL DEFAULT 0;
