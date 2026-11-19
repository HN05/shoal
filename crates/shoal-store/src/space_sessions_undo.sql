-- What tests that emulate an earlier database run first to undo
-- space_sessions.sql: a chat's row has a column for its agent's session.
ALTER TABLE chats ADD COLUMN session TEXT;
