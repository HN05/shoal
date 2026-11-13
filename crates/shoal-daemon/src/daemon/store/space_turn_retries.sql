-- A chat's agent has the turns an API failure ended retried as a
-- workspace's does, so the retry names the space, a workspace or chat. It
-- lasts through the space's conversion to the other kind.
CREATE TABLE space_turn_retries (
    space_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    failure TEXT NOT NULL CHECK(failure IN ('rate_limited','overloaded','server_error')),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    due_at INTEGER NOT NULL,
    delivered INTEGER NOT NULL CHECK(delivered IN (0,1))
);
INSERT INTO space_turn_retries(space_id,execution_id,failure,attempt,due_at,delivered)
    SELECT workspace_id,execution_id,failure,attempt,due_at,delivered
    FROM workspace_turn_retries;
DROP TABLE workspace_turn_retries;
CREATE INDEX space_turn_retries_execution ON space_turn_retries(execution_id);

CREATE TRIGGER workspace_turn_retry_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_turn_retry_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
END;
