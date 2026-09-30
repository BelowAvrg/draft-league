-- The trail left by an admin undoing a pick. The pick row itself is deleted so
-- the Pokémon and points return to the draft; this row records what it was.
--
-- Keyed by person rather than coach so removing a coach from the season later
-- is not blocked by their undone picks.
CREATE TABLE pick_correction (
    id          INTEGER PRIMARY KEY,
    season_id   INTEGER NOT NULL REFERENCES season (id),
    person_id   INTEGER NOT NULL REFERENCES person (id),
    pokemon_id  INTEGER NOT NULL REFERENCES pokemon (id),
    pick_number INTEGER NOT NULL,
    points_paid INTEGER NOT NULL,
    picked_at   TEXT NOT NULL,
    undone_by   INTEGER NOT NULL REFERENCES person (id),
    undone_at   TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;
