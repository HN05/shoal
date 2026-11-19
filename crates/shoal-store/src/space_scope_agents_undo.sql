-- What tests that emulate an earlier database run first to undo
-- space_scope_agents.sql.
ALTER TABLE space_scopes DROP COLUMN agent;
