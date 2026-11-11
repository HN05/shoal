-- The space whose scoped process queued a launch of a new workspace. The
-- launch opens its workspace in that space's scope, keeps the space while
-- it waits and goes with it: removing the space deletes the launches it
-- queued that have not opened their workspace yet. A conversion, which
-- moves the space's row to the other table, keeps them.
ALTER TABLE agent_queue ADD COLUMN scope_space_id TEXT;
CREATE INDEX agent_queue_scopes ON agent_queue(scope_space_id);
CREATE TRIGGER workspace_queued_launches_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND workspace_id IS NULL;
END;
CREATE TRIGGER chat_queued_launches_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND workspace_id IS NULL;
END;
