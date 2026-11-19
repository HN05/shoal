-- The retry of a turn an API failure ended for a workspace's agent, which
-- goes with the reporting execution's record. It waits until `due_at`, then
-- is `delivered` once the agent is asked to continue, until that turn ends.
CREATE TABLE workspace_turn_retries (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    failure TEXT NOT NULL CHECK(failure IN ('rate_limited','overloaded','server_error')),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    due_at INTEGER NOT NULL,
    delivered INTEGER NOT NULL CHECK(delivered IN (0,1))
);
CREATE INDEX workspace_turn_retries_execution ON workspace_turn_retries(execution_id);
