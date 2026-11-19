-- What tests that emulate an earlier database run first to undo
-- agent_queue_scopes.sql: queued launches record no space that queued them.
DROP TRIGGER workspace_queued_launches_removed;
DROP TRIGGER chat_queued_launches_removed;
DROP INDEX agent_queue_scopes;
ALTER TABLE agent_queue DROP COLUMN scope_space_id;
