-- What tests that emulate an earlier database run first to undo
-- triggers.sql: triggers keep no runtime state.
DROP TRIGGER repository_triggers_removed;
DROP TABLE trigger_keys;
DROP TABLE trigger_runs;
DROP TABLE trigger_states;
