-- Unread messages for a chat about the workspaces it coordinates; delivery
-- deletes them. `kind` is the notification the user receives instead when
-- the chat ends first.
CREATE TABLE IF NOT EXISTS chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    workspace TEXT NOT NULL,
    kind TEXT,
    message TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS chat_messages_chat ON chat_messages(chat_id, id);
