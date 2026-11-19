-- What tests that emulate an earlier database run first to undo
-- chat_places.sql: a chat's row has a column for its place.
ALTER TABLE chats ADD COLUMN origin TEXT;
