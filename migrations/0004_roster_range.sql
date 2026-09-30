-- Roster size becomes a range (8-12) and a coach can finish early.
--
-- SQLite cannot add a CHECK constraint to an existing table, and the pair
-- constraint (min <= max) spans two columns, so season is rebuilt. No season
-- data is worth preserving at this point, but the copy keeps it anyway.

PRAGMA foreign_keys = OFF;

CREATE TABLE season_new (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    min_roster  INTEGER NOT NULL CHECK (min_roster > 0),
    max_roster  INTEGER NOT NULL,
    is_active   INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1)),
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK (min_roster <= max_roster)
) STRICT;

-- The old single size becomes the maximum; 8 is this league's minimum.
INSERT INTO season_new (id, name, min_roster, max_roster, is_active, created_at)
SELECT id, name, MIN(8, roster_size), roster_size, is_active, created_at FROM season;

DROP TABLE season;
ALTER TABLE season_new RENAME TO season;

CREATE UNIQUE INDEX season_one_active ON season (is_active) WHERE is_active = 1;

PRAGMA foreign_keys = ON;

-- NULL means still drafting. A timestamp rather than a flag so the admin
-- undoing it has a trail of when the coach stopped.
ALTER TABLE coach ADD COLUMN done_at TEXT;
