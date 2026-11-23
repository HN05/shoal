-- What tests that emulate an earlier database run first to undo
-- agent_lease_spaces.sql.
ALTER TABLE resource_leases RENAME COLUMN acquirer_id TO acquirer_execution;
ALTER TABLE container_projects RENAME COLUMN acquirer_id TO acquirer_execution;
