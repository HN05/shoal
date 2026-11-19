-- What tests that emulate an earlier database run first to undo
-- agent_queue_sources.sql: queued launches record no source.
ALTER TABLE agent_queue DROP COLUMN source;
