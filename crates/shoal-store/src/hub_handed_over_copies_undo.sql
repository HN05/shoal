-- What tests that emulate an earlier database run first to undo
-- hub_handed_over_copies.sql.
ALTER TABLE hub_space_copies DROP COLUMN handed_over;
