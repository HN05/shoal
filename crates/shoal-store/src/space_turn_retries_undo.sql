-- What tests that emulate an earlier database run first to undo
-- space_turn_retries.sql: only a workspace's turns are retried.
CREATE TABLE workspace_turn_retries (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    failure TEXT NOT NULL CHECK(failure IN ('rate_limited','overloaded','server_error')),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    due_at INTEGER NOT NULL,
    delivered INTEGER NOT NULL CHECK(delivered IN (0,1))
);
CREATE INDEX workspace_turn_retries_execution ON workspace_turn_retries(execution_id);
INSERT INTO workspace_turn_retries(workspace_id,execution_id,failure,attempt,due_at,delivered)
    SELECT space_id,execution_id,failure,attempt,due_at,delivered FROM space_turn_retries
    WHERE space_id IN (SELECT id FROM workspaces);
DROP TRIGGER workspace_turn_retry_removed;
DROP TRIGGER chat_turn_retry_removed;
DROP TABLE space_turn_retries;
