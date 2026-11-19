-- What tests that emulate an earlier database run first to undo
-- execution_spaces.sql: executions and queue entries name their workspace
-- again. The references the upgrade dropped stay dropped.
DROP TRIGGER workspace_executions_removed;
DROP TRIGGER chat_executions_removed;
DROP INDEX executions_space;
ALTER TABLE executions RENAME COLUMN space_id TO workspace_id;
CREATE INDEX executions_workspace ON executions(workspace_id);
DROP INDEX agent_queue_space;
ALTER TABLE agent_queue RENAME COLUMN space_id TO workspace_id;
CREATE INDEX agent_queue_workspace ON agent_queue(workspace_id);
