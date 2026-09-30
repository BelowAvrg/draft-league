-- Season play: team names and the schedule. See SEASON.md.

-- The schedule export names teams, not people, so import matches on this.
ALTER TABLE coach ADD COLUMN team_name TEXT;
CREATE UNIQUE INDEX coach_team_name ON coach (season_id, team_name) WHERE team_name IS NOT NULL;

-- Games per match. Odd, so a match always has a winner.
ALTER TABLE season ADD COLUMN best_of INTEGER NOT NULL DEFAULT 3
    CHECK (best_of >= 1 AND best_of % 2 = 1);

-- One scheduled pairing. Playoff matches start with both coach slots NULL and
-- are filled by seeding; a composite foreign key with a NULL column is not
-- checked, and once filled it pins each coach to this match's season.
--
-- differential is the winner's, and may be negative: a 2-1 winner can lose
-- the third game by more than they won the first two.
CREATE TABLE match (
    id              INTEGER PRIMARY KEY,
    season_id       INTEGER NOT NULL REFERENCES season (id),
    week            INTEGER NOT NULL CHECK (week > 0),
    is_playoff      INTEGER NOT NULL DEFAULT 0 CHECK (is_playoff IN (0, 1)),
    coach_a_id      INTEGER,
    coach_b_id      INTEGER,
    winner_coach_id INTEGER,
    differential    INTEGER,
    is_forfeit      INTEGER NOT NULL DEFAULT 0 CHECK (is_forfeit IN (0, 1)),
    result_source   TEXT CHECK (result_source IN ('replay', 'manual')),
    FOREIGN KEY (coach_a_id, season_id) REFERENCES coach (id, season_id),
    FOREIGN KEY (coach_b_id, season_id) REFERENCES coach (id, season_id),
    CHECK (coach_a_id <> coach_b_id),
    CHECK (winner_coach_id IS NULL OR winner_coach_id IN (coach_a_id, coach_b_id)),
    -- A result is winner, differential, and source together, or none of them.
    CHECK ((winner_coach_id IS NULL) = (differential IS NULL)),
    CHECK ((winner_coach_id IS NULL) = (result_source IS NULL)),
    CHECK (is_forfeit = 0 OR winner_coach_id IS NOT NULL)
) STRICT;

CREATE INDEX match_season_week ON match (season_id, week);
