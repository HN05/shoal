-- What tests that emulate an earlier database run first to undo
-- lifecycle_generations.sql.
DROP TRIGGER workspace_lifecycle_changed;
ALTER TABLE workspaces DROP COLUMN lifecycle_generation;
