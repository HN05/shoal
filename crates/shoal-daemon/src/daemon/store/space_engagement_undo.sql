-- What tests that emulate an earlier database run first to undo
-- space_engagement.sql.
DROP TABLE reply_times;
DROP TRIGGER space_engagement_removed;
DROP TABLE space_engagement;
