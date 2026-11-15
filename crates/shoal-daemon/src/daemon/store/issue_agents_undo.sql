-- What tests that emulate an earlier database run first to undo
-- issue_agents.sql: an issue's assignee records the account that assigned it.
ALTER TABLE issue_assignments ADD COLUMN account TEXT NOT NULL DEFAULT 'user';
ALTER TABLE space_issue RENAME COLUMN linked_by TO assigner;
