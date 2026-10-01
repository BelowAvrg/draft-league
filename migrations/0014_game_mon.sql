-- Per-Pokémon stats read from each game's replay log. See STATS.md.
--
-- One row per previewed Pokémon. Everything shown is derived from these rows
-- at page load; nothing is aggregated here.
CREATE TABLE game_mon (
    id          INTEGER PRIMARY KEY,
    game_id     INTEGER NOT NULL REFERENCES game (id) ON DELETE CASCADE,
    coach_id    INTEGER NOT NULL REFERENCES coach (id),
    -- NULL when it couldn't be matched to the coach's roster that week.
    pokemon_id  INTEGER REFERENCES pokemon (id),
    -- Showdown's name for it, final form.
    species     TEXT NOT NULL,
    played      INTEGER NOT NULL CHECK (played IN (0, 1)),
    led         INTEGER NOT NULL CHECK (led IN (0, 1)),
    mega        INTEGER NOT NULL CHECK (mega IN (0, 1)),
    fainted     INTEGER NOT NULL CHECK (fainted IN (0, 1)),
    direct_kos  INTEGER NOT NULL CHECK (direct_kos >= 0),
    passive_kos INTEGER NOT NULL CHECK (passive_kos >= 0),
    UNIQUE (game_id, coach_id, species)
) STRICT;

CREATE INDEX game_mon_coach ON game_mon (coach_id);
CREATE INDEX game_mon_pokemon ON game_mon (pokemon_id);

-- Moves, items and abilities a game showed a Pokémon with.
CREATE TABLE game_mon_reveal (
    game_mon_id INTEGER NOT NULL REFERENCES game_mon (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('move', 'item', 'ability')),
    name        TEXT NOT NULL,
    PRIMARY KEY (game_mon_id, kind, name)
) STRICT, WITHOUT ROWID;
