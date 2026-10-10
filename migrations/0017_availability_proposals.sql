-- Recurring weekly availability and match-time proposals.

-- IANA zone such as America/Denver. NULL until the member saves availability.
ALTER TABLE person ADD COLUMN time_zone TEXT;

-- One recurring half-hour block in the person's local time.
CREATE TABLE person_availability (
    person_id INTEGER NOT NULL REFERENCES person (id) ON DELETE CASCADE,
    weekday   INTEGER NOT NULL CHECK (weekday BETWEEN 0 AND 6),
    slot       INTEGER NOT NULL CHECK (slot BETWEEN 0 AND 47),
    PRIMARY KEY (person_id, weekday, slot)
) STRICT, WITHOUT ROWID;

-- A set of concrete times offered for one match. History is retained after
-- the proposal is accepted, declined, withdrawn, or superseded.
CREATE TABLE match_time_proposal (
    id                    INTEGER PRIMARY KEY,
    match_id              INTEGER NOT NULL REFERENCES match (id) ON DELETE CASCADE,
    proposed_by_person_id INTEGER NOT NULL REFERENCES person (id),
    status                TEXT NOT NULL DEFAULT 'open'
                          CHECK (status IN ('open', 'accepted', 'declined', 'withdrawn', 'superseded')),
    decided_by_person_id  INTEGER REFERENCES person (id),
    created_at            TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at            TEXT,
    CHECK ((status = 'open') = (decided_at IS NULL)),
    CHECK ((status = 'open') = (decided_by_person_id IS NULL))
) STRICT;

CREATE UNIQUE INDEX match_one_open_time_proposal
    ON match_time_proposal (match_id) WHERE status = 'open';

CREATE INDEX match_time_proposal_match ON match_time_proposal (match_id, id DESC);

CREATE TABLE match_time_option (
    id          INTEGER PRIMARY KEY,
    proposal_id INTEGER NOT NULL REFERENCES match_time_proposal (id) ON DELETE CASCADE,
    starts_at   TEXT NOT NULL CHECK (datetime(starts_at) IS starts_at),
    accepted    INTEGER NOT NULL DEFAULT 0 CHECK (accepted IN (0, 1)),
    UNIQUE (proposal_id, starts_at)
) STRICT;

CREATE UNIQUE INDEX match_time_option_one_accepted
    ON match_time_option (proposal_id) WHERE accepted = 1;
