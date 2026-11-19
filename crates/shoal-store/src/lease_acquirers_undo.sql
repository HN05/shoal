-- What tests that emulate an earlier database run first to undo
-- lease_acquirers.sql.
DROP INDEX resource_lease_acquirers;
ALTER TABLE resource_leases DROP COLUMN acquirer_execution;
ALTER TABLE resource_leases DROP COLUMN acquirer;
