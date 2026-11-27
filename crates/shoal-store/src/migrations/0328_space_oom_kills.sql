-- The OOM kills that ended a space's agent since it last finished a turn,
-- which back off its restores, and the execution of the latest, so that a
-- kill counts once whichever reports it.
CREATE TABLE space_oom_kills (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    kills INTEGER NOT NULL CHECK(kills > 0),
    execution_id TEXT NOT NULL
) WITHOUT ROWID;
