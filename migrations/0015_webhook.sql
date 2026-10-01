-- Discord webhook URL per event kind. No row = that kind is not posted.
-- The URL is a credential; the admin page shows only whether it is set.
-- See DISCORD.md.
CREATE TABLE webhook (
    kind TEXT PRIMARY KEY CHECK (kind IN ('draft', 'schedule', 'replay', 'trade')),
    url  TEXT NOT NULL
) STRICT;
