-- The raw replay log, so stats can be read (and re-read) without fetching.
-- NULL for games uploaded before this column existed. See STATS.md.
ALTER TABLE game ADD COLUMN log TEXT;
