-- What tests that emulate an earlier database run first to undo
-- review_sessions.sql: reviews keep no sessions.
ALTER TABLE reviews DROP COLUMN session;
