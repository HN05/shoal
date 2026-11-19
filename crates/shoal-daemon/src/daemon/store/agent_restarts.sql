-- A restart of a space's agent the daemon took for later, until it
-- restarts the agent, the agent stops or exits first, the space goes or
-- the restart is cancelled: `restart` holds how it restarts, `asked_by`
-- the space whose process asked for it, none for the user, and
-- `recorded_at` its place among the sequential restarts.
CREATE TABLE agent_restarts (
    space_id TEXT PRIMARY KEY,
    restart TEXT NOT NULL,
    asked_by TEXT,
    recorded_at INTEGER NOT NULL
);
CREATE TRIGGER workspace_restart_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM agent_restarts WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_restart_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM agent_restarts WHERE space_id=OLD.id;
END;
