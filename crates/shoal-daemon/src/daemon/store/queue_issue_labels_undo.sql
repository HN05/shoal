-- What tests that emulate an earlier database run first to undo
-- queue_issue_labels.sql: queued launches record no issue labels.
ALTER TABLE agent_queue DROP COLUMN issue_labels;
