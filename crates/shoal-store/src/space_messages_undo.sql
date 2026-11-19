-- What tests that emulate an earlier database run first to undo
-- space_messages.sql: workspaces and chats keep their messages apart.
CREATE TABLE agent_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    message TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1))
);
CREATE INDEX agent_messages_workspace ON agent_messages(workspace_id, id);
CREATE TABLE chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    workspace TEXT NOT NULL,
    kind TEXT,
    message TEXT NOT NULL,
    workspace_id TEXT,
    attachments TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id, id);
INSERT INTO agent_messages(id,workspace_id,created_at,message,attachments,wakes)
    SELECT id,space_id,created_at,message,attachments,wakes FROM space_messages
    WHERE space_id IN (SELECT id FROM workspaces);
INSERT INTO chat_messages(id,chat_id,created_at,workspace,kind,message,workspace_id,attachments)
    SELECT id,space_id,created_at,COALESCE(about,''),kind,message,about_id,attachments
    FROM space_messages WHERE space_id IN (SELECT id FROM chats);
DROP TRIGGER space_messages_removed;
DROP TABLE space_messages;
