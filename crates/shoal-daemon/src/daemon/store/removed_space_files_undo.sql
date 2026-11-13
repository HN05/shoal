-- What tests that emulate an earlier database run first to undo
-- removed_space_files.sql, before file_events.sql restores the triggers it
-- replaced: only removed chats record how long their files stay.
DROP TRIGGER file_added;
DROP TRIGGER file_removed;
DROP TABLE removed_space_files;
CREATE TABLE ended_chats (
    chat_id TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);
