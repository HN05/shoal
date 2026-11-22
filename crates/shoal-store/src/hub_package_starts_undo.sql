-- What tests that emulate an earlier database run first to undo
-- hub_package_starts.sql.
ALTER TABLE hub_packages DROP COLUMN within;
ALTER TABLE hub_packages DROP COLUMN start;
