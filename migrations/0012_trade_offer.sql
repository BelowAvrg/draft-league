-- Trade offers between two coaches. See TRADES.md.

-- Private to its two coaches. An accept creates the move; reason says why an
-- offer was declined for the coaches when the site declined it itself.
CREATE TABLE trade_offer (
    id              INTEGER PRIMARY KEY,
    season_id       INTEGER NOT NULL REFERENCES season (id),
    from_coach_id   INTEGER NOT NULL,
    to_coach_id     INTEGER NOT NULL,
    status          TEXT NOT NULL DEFAULT 'open'
                    CHECK (status IN ('open', 'accepted', 'declined', 'withdrawn')),
    reason          TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at      TEXT,
    move_id         INTEGER REFERENCES move (id),
    FOREIGN KEY (from_coach_id, season_id) REFERENCES coach (id, season_id),
    FOREIGN KEY (to_coach_id, season_id) REFERENCES coach (id, season_id),
    CHECK (from_coach_id <> to_coach_id),
    CHECK ((status = 'accepted') = (move_id IS NOT NULL)),
    CHECK ((status = 'open') = (decided_at IS NULL))
) STRICT;

CREATE INDEX trade_offer_to ON trade_offer (to_coach_id, status);
CREATE INDEX trade_offer_from ON trade_offer (from_coach_id, status);

-- What each side gives. from_coach_id is whichever of the offer's two coaches
-- owned it when the offer was made.
CREATE TABLE trade_offer_item (
    offer_id        INTEGER NOT NULL REFERENCES trade_offer (id),
    pokemon_id      INTEGER NOT NULL REFERENCES pokemon (id),
    from_coach_id   INTEGER NOT NULL REFERENCES coach (id),
    PRIMARY KEY (offer_id, pokemon_id)
) STRICT;

-- Finding the open offers a moved Pokémon kills.
CREATE INDEX trade_offer_item_pokemon ON trade_offer_item (pokemon_id);

-- The move that took this Pokémon off this coach's roster, so the move log
-- can show a free agency drop as well as the pickup.
ALTER TABLE roster_entry ADD COLUMN left_move_id INTEGER REFERENCES move (id);
