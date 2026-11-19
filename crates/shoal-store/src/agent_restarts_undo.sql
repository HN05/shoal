-- What tests that emulate an earlier database run first to undo
-- agent_restarts.sql.
DROP TRIGGER workspace_restart_removed;
DROP TRIGGER chat_restart_removed;
DROP TABLE agent_restarts;
