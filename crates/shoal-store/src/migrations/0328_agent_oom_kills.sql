-- How often the OOM killer killed each space's agent since it last finished
-- a turn, and when it last did, which hold back its next restore. The
-- execution its last kill ended lets that kill count once, whether its wrapper
-- reported it or its recovery record names it.
CREATE TABLE agent_oom_kills (
    space_id TEXT PRIMARY KEY,
    kills INTEGER NOT NULL CHECK(kills > 0),
    killed_at INTEGER NOT NULL,
    execution_id TEXT NOT NULL
) WITHOUT ROWID;

CREATE TRIGGER space_oom_kills_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM agent_oom_kills WHERE space_id=OLD.id;
END;
