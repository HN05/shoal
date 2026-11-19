-- When the user last interacted with a space's agent, and when its agent's
-- last turn and the turn before it ended, in Unix seconds: the user is
-- engaged with the agent while they interacted since the turn before the
-- last ended. It goes with the space.
CREATE TABLE space_engagement (
    space_id TEXT PRIMARY KEY,
    interacted_at INTEGER,
    turn_ended_at INTEGER,
    previous_turn_ended_at INTEGER
);

CREATE TRIGGER space_engagement_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_engagement WHERE space_id=OLD.id;
END;

-- How long the user took from the end of a turn they were engaged with to
-- their first interaction after it, in seconds, newest last. They outlive
-- their space, since they also make the user's reply time over every space.
CREATE TABLE reply_times (
    id INTEGER PRIMARY KEY,
    space_id TEXT NOT NULL,
    seconds INTEGER NOT NULL
);
CREATE INDEX reply_times_space ON reply_times(space_id, id);
