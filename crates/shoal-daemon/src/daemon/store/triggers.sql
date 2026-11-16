-- Each trigger's runtime state, by where it is configured, a registered
-- repository's ID or 'global' for the global config, and its name, so that
-- it outlives reloads, restarts and config edits. A source counts the times
-- it would fire after `counted_from`; `last_fired_at` is the last firing
-- that reached a run.
CREATE TABLE trigger_states (
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
    created_at INTEGER NOT NULL,
    counted_from INTEGER NOT NULL,
    last_fired_at INTEGER,
    PRIMARY KEY(scope, name)
);

-- Each trigger's newest runs, with the space a started run started, or the
-- agent queue entry of its new space's launch until that opens the space. A
-- queued run keeps the launch it waits to make, as JSON, and the space or
-- entry it waits for, until it starts.
CREATE TABLE trigger_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    at INTEGER NOT NULL,
    origin TEXT NOT NULL CHECK(origin IN ('source','manual')),
    outcome TEXT NOT NULL CHECK(outcome IN ('started','queued','skipped','duplicate','failed')),
    reason TEXT,
    space_id TEXT,
    queue_entry TEXT,
    launch TEXT
);
CREATE INDEX trigger_runs_by_trigger ON trigger_runs(scope, name, id);

-- The keys of the runs a trigger started, each until its window ends.
CREATE TABLE trigger_keys (
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    key TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY(scope, name, key)
);

CREATE TRIGGER repository_triggers_removed AFTER DELETE ON repositories BEGIN
    DELETE FROM trigger_states WHERE scope=OLD.id;
    DELETE FROM trigger_runs WHERE scope=OLD.id;
    DELETE FROM trigger_keys WHERE scope=OLD.id;
END;
