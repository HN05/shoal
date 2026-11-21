-- What tests that emulate an earlier database run first to undo
-- hub_drop_alerts.sql.
ALTER TABLE hub_members DROP COLUMN drop_alerted;
