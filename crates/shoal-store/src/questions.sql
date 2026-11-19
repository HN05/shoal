-- Questions a workspace's agents ask the user; removal deletes them.
CREATE TABLE IF NOT EXISTS questions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    question TEXT NOT NULL,
    recommendation TEXT NOT NULL,
    reason TEXT,
    choices TEXT NOT NULL,
    answer TEXT,
    answered_at INTEGER,
    message_id INTEGER
);
CREATE INDEX IF NOT EXISTS questions_workspace ON questions(workspace_id, id);
