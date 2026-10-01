-- Post-draft roster moves: trades and free agency. See TRADES.md.

-- One move. A free agency move has one coach; a trade has two. The Pokémon
-- that changed hands are the roster_entry rows pointing here.
CREATE TABLE move (
    id              INTEGER PRIMARY KEY,
    season_id       INTEGER NOT NULL REFERENCES season (id),
    kind            TEXT NOT NULL CHECK (kind IN ('trade', 'free_agency')),
    coach_a_id      INTEGER NOT NULL,
    coach_b_id      INTEGER,
    -- First week the new rosters play.
    effective_week  INTEGER NOT NULL CHECK (effective_week > 1),
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (coach_a_id, season_id) REFERENCES coach (id, season_id),
    FOREIGN KEY (coach_b_id, season_id) REFERENCES coach (id, season_id),
    CHECK ((kind = 'trade') = (coach_b_id IS NOT NULL)),
    CHECK (coach_a_id <> coach_b_id)
) STRICT;

CREATE INDEX move_season ON move (season_id);

-- The move that brought this Pokémon to this coach; NULL means drafted.
ALTER TABLE roster_entry ADD COLUMN move_id INTEGER REFERENCES move (id);
