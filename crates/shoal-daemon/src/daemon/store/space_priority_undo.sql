-- What tests that emulate an earlier database run first to undo
-- space_priority.sql: spaces and queued launches record no priority.
DROP TRIGGER workspace_priority_removed;
DROP TRIGGER chat_priority_removed;
DROP TABLE space_priority;
ALTER TABLE agent_queue DROP COLUMN priority;
