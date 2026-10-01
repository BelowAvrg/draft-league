-- Who owns each Pokémon, week by week. See TRADES.md.
--
-- pick stays the draft's append-only record; after the draft, trades and free
-- agency move ownership here. A drafted Pokémon is owned from week 1.

CREATE TABLE roster_entry (
    id          INTEGER PRIMARY KEY,
    season_id   INTEGER NOT NULL REFERENCES season (id),
    coach_id    INTEGER NOT NULL,
    pokemon_id  INTEGER NOT NULL REFERENCES pokemon (id),
    from_week   INTEGER NOT NULL CHECK (from_week > 0),
    -- NULL means still owned. Otherwise the first week it no longer plays.
    until_week  INTEGER CHECK (until_week > from_week),
    FOREIGN KEY (coach_id, season_id) REFERENCES coach (id, season_id)
) STRICT;

-- One owner at a time.
CREATE UNIQUE INDEX roster_entry_owner ON roster_entry (season_id, pokemon_id)
    WHERE until_week IS NULL;

CREATE INDEX roster_entry_coach ON roster_entry (coach_id);

INSERT INTO roster_entry (season_id, coach_id, pokemon_id, from_week)
SELECT season_id, coach_id, pokemon_id, 1 FROM pick;

-- The draft writes pick; these keep ownership in step without it knowing.
CREATE TRIGGER pick_owns AFTER INSERT ON pick
BEGIN
    INSERT INTO roster_entry (season_id, coach_id, pokemon_id, from_week)
    VALUES (NEW.season_id, NEW.coach_id, NEW.pokemon_id, 1);
END;

-- Undoing a pick only makes sense while the drafter still holds it. Once it
-- has moved, deleting the pick would leave the new owner's entry orphaned.
CREATE TRIGGER pick_disowns AFTER DELETE ON pick
BEGIN
    SELECT RAISE(ABORT, 'pick has since changed hands')
    WHERE EXISTS (
        SELECT 1 FROM roster_entry
        WHERE season_id = OLD.season_id AND pokemon_id = OLD.pokemon_id
          AND until_week IS NOT NULL
    );
    DELETE FROM roster_entry
    WHERE season_id = OLD.season_id AND pokemon_id = OLD.pokemon_id;
END;
