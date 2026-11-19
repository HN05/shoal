-- Who acquired each resource lease: the user, whose lease stays until
-- released, or the agent or demo execution it ends with. Leases recorded
-- before this are the user's. No foreign key: the execution's record goes
-- when it ends, and the lease is released after.
ALTER TABLE resource_leases ADD COLUMN acquirer TEXT NOT NULL DEFAULT 'user'
    CHECK(acquirer IN ('user','agent','demo'));
ALTER TABLE resource_leases ADD COLUMN acquirer_execution TEXT
    CHECK((acquirer='user') = (acquirer_execution IS NULL));
CREATE INDEX resource_lease_acquirers ON resource_leases(acquirer_execution)
    WHERE acquirer_execution IS NOT NULL;
