-- The timer `shoal sleep --for` or `--until` set for a space, which sends
-- its agent `message`, or says only that it fired, at `due_at`.
CREATE TABLE space_wake_timers (
    space_id TEXT PRIMARY KEY,
    due_at INTEGER NOT NULL,
    message TEXT
);

CREATE TRIGGER space_wake_timer_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_wake_timers WHERE space_id=OLD.id;
END;
