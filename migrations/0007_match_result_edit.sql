-- Every write to a match result, including clearing one. See SEASON.md.
--
-- The old and new results are copied rather than referenced, so the trail
-- still reads correctly after later edits.
CREATE TABLE match_result_edit (
    id                  INTEGER PRIMARY KEY,
    match_id            INTEGER NOT NULL REFERENCES match (id),
    person_id           INTEGER NOT NULL REFERENCES person (id),
    source              TEXT NOT NULL CHECK (source IN ('replay', 'manual')),
    old_winner_coach_id INTEGER,
    old_differential    INTEGER,
    old_is_forfeit      INTEGER NOT NULL CHECK (old_is_forfeit IN (0, 1)),
    new_winner_coach_id INTEGER,
    new_differential    INTEGER,
    new_is_forfeit      INTEGER NOT NULL CHECK (new_is_forfeit IN (0, 1)),
    created_at          TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;

CREATE INDEX match_result_edit_match ON match_result_edit (match_id);
