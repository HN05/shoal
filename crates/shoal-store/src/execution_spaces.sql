-- An execution, and the agent-queue entry that restores or opened one,
-- belong to a space, a workspace or a chat, whose IDs never collide, so
-- neither references a space table. Removing a space removes them, as
-- removing a workspace did. A conversion moves the space's row to the
-- other table, which keeps them: a chat that becomes a workspace carries
-- its own over, while a workspace that becomes a chat removes them itself.
ALTER TABLE executions RENAME COLUMN workspace_id TO space_id;
DROP INDEX executions_workspace;
CREATE INDEX executions_space ON executions(space_id);
ALTER TABLE agent_queue RENAME COLUMN workspace_id TO space_id;
DROP INDEX agent_queue_workspace;
CREATE INDEX agent_queue_space ON agent_queue(space_id);

CREATE TRIGGER workspace_executions_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_executions_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
END;
