-- When a match is to be played. See DISCORD.md.
-- UTC as 'YYYY-MM-DD HH:MM:SS'; NULL = unscheduled. datetime() returns its
-- argument unchanged only when it is already in that form.
ALTER TABLE match ADD COLUMN scheduled_at TEXT CHECK (datetime(scheduled_at) IS scheduled_at);
