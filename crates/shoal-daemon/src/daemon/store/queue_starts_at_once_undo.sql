-- What tests that emulate an earlier database run first to undo
-- queue_starts_at_once.sql.
ALTER TABLE agent_queue DROP COLUMN starts_at_once;
