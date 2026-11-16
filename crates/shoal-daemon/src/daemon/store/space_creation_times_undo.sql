-- What tests that emulate an earlier database run first to undo
-- space_creation_times.sql: a chat's row records when it started.
ALTER TABLE chats ADD COLUMN started_at INTEGER NOT NULL DEFAULT 0;
UPDATE chats SET started_at=(SELECT created_at FROM spaces WHERE id=chats.id);
ALTER TABLE spaces DROP COLUMN created_at;
