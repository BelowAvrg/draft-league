-- Session store backing tower-sessions.
-- id is the session Id (an i128) as hex text, since SQLite has no i128.
CREATE TABLE session (
    id          TEXT PRIMARY KEY,
    data        BLOB NOT NULL,
    expiry_date TEXT NOT NULL
) STRICT;

CREATE INDEX session_expiry ON session (expiry_date);
