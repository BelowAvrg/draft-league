-- Core draft schema. See DESIGN.md for the decisions behind it.

PRAGMA foreign_keys = ON;

CREATE TABLE person (
    id                INTEGER PRIMARY KEY,
    discord_id        TEXT NOT NULL UNIQUE,
    discord_username  TEXT NOT NULL,
    showdown_username TEXT,
    is_admin          INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1))
) STRICT;

CREATE TABLE season (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    roster_size INTEGER NOT NULL CHECK (roster_size > 0),
    is_active   INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1)),
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;

-- Exactly one active season. A partial unique index lets many rows hold 0
-- while at most one holds 1.
CREATE UNIQUE INDEX season_one_active ON season (is_active) WHERE is_active = 1;

CREATE TABLE coach (
    id             INTEGER PRIMARY KEY,
    person_id      INTEGER NOT NULL REFERENCES person (id),
    season_id      INTEGER NOT NULL REFERENCES season (id),
    budget         INTEGER NOT NULL CHECK (budget > 0),
    draft_position INTEGER NOT NULL CHECK (draft_position > 0),
    UNIQUE (person_id, season_id),
    UNIQUE (season_id, draft_position)
) STRICT;

CREATE TABLE pokemon (
    id           INTEGER PRIMARY KEY,
    slug         TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL
) STRICT;

-- The tier list: per-season prices over the stable pokemon list.
CREATE TABLE cost (
    season_id  INTEGER NOT NULL REFERENCES season (id),
    pokemon_id INTEGER NOT NULL REFERENCES pokemon (id),
    points     INTEGER NOT NULL CHECK (points >= 1),
    PRIMARY KEY (season_id, pokemon_id)
) STRICT;

-- season_id is denormalized from coach so "drafted once per season" can be a
-- database constraint rather than application logic. The composite foreign key
-- keeps it honest: it must match the coach's own season.
CREATE TABLE pick (
    id          INTEGER PRIMARY KEY,
    coach_id    INTEGER NOT NULL,
    season_id   INTEGER NOT NULL REFERENCES season (id),
    pokemon_id  INTEGER NOT NULL REFERENCES pokemon (id),
    pick_number INTEGER NOT NULL CHECK (pick_number > 0),
    points_paid INTEGER NOT NULL CHECK (points_paid >= 1),
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (season_id, pokemon_id),
    UNIQUE (coach_id, pick_number),
    FOREIGN KEY (coach_id, season_id) REFERENCES coach (id, season_id)
) STRICT;

-- Needed as the target of pick's composite foreign key.
CREATE UNIQUE INDEX coach_id_season ON coach (id, season_id);

CREATE INDEX pick_season_number ON pick (season_id, pick_number);

-- Contents are private to the owning coach; only the count is public.
CREATE TABLE queue_slot (
    coach_id    INTEGER NOT NULL REFERENCES coach (id),
    slot_number INTEGER NOT NULL CHECK (slot_number > 0),
    pokemon_id  INTEGER NOT NULL REFERENCES pokemon (id),
    PRIMARY KEY (coach_id, slot_number)
) STRICT;
