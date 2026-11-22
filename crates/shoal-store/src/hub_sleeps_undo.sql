-- What tests that emulate an earlier database run first to undo
-- hub_sleeps.sql.
DROP INDEX hub_sleep_lags_member;
DROP TABLE hub_sleep_lags;
ALTER TABLE hub_members DROP COLUMN asleep_at;
