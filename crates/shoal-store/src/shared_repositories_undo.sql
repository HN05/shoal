-- What tests that emulate an earlier database run first to undo
-- shared_repositories.sql.
ALTER TABLE repositories DROP COLUMN shared_url;
