-- A chat's agent asks questions as a workspace's does: each question belongs
-- to exactly one workspace or chat, and goes with it.
CREATE TABLE questions_of_spaces (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    chat_id TEXT REFERENCES chats(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    question TEXT NOT NULL,
    recommendation TEXT NOT NULL,
    reason TEXT,
    choices TEXT NOT NULL,
    answer TEXT,
    answered_at INTEGER,
    message_id INTEGER,
    closed_at INTEGER,
    number INTEGER NOT NULL DEFAULT 0,
    withdrawn INTEGER NOT NULL DEFAULT 0 CHECK(withdrawn IN (0,1)),
    in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1)),
    CHECK((workspace_id IS NULL) <> (chat_id IS NULL))
);
INSERT INTO questions_of_spaces(id,workspace_id,created_at,question,recommendation,reason,
        choices,answer,answered_at,message_id,closed_at,number,withdrawn,in_chat)
    SELECT id,workspace_id,created_at,question,recommendation,reason,choices,answer,
        answered_at,message_id,closed_at,number,withdrawn,in_chat
    FROM questions;
DROP TABLE questions;
ALTER TABLE questions_of_spaces RENAME TO questions;
CREATE INDEX questions_workspace ON questions(workspace_id, id);
CREATE INDEX questions_chat ON questions(chat_id, id);
CREATE UNIQUE INDEX questions_number ON questions(number);

-- Whether the chat's agent marked its current turn as asking the user in
-- its conversation, as an execution's mark does.
ALTER TABLE chats ADD COLUMN asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1));
