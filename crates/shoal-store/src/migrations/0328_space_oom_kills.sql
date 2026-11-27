-- The OOM kills that ended a space's agent since it last finished a turn,
-- which back off its restores.
CREATE TABLE space_oom_kills (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    kills INTEGER NOT NULL CHECK(kills > 0)
) WITHOUT ROWID;
