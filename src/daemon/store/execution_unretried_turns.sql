-- An execution whose agent's failed turn Shoal told of without retrying it,
-- until the agent ends a turn normally, so later failed turns add nothing.
CREATE TABLE execution_unretried_turns (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE
);
