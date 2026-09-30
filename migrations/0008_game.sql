-- Replays attached to matches as games. See SEASON.md.

-- Showdown format id every replay in the season must be played in. NULL skips
-- the check.
ALTER TABLE season ADD COLUMN format TEXT;
UPDATE season SET format = 'gen9championsvgc2026regmc';

-- One replayed game. Remaining counts are per coach slot of the match, which
-- seeding will not move once a game exists.
CREATE TABLE game (
    id              INTEGER PRIMARY KEY,
    match_id        INTEGER NOT NULL REFERENCES match (id),
    -- Showdown's replay id, without any private-replay password.
    replay_id       TEXT NOT NULL UNIQUE,
    replay_url      TEXT NOT NULL,
    winner_coach_id INTEGER NOT NULL REFERENCES coach (id),
    a_remaining     INTEGER NOT NULL CHECK (a_remaining >= 0),
    b_remaining     INTEGER NOT NULL CHECK (b_remaining >= 0),
    created_by      INTEGER NOT NULL REFERENCES person (id),
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;

CREATE INDEX game_match ON game (match_id);
