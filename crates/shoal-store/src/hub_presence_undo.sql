-- What tests that emulate an earlier database run first to undo
-- hub_presence.sql.
ALTER TABLE hub_members DROP COLUMN present_since;
ALTER TABLE hub_members DROP COLUMN availability;
ALTER TABLE hub_members DROP COLUMN battery;
DROP TABLE hub_presence;
