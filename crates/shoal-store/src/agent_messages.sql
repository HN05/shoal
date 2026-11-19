-- Unread messages for a workspace's agents; delivery deletes them.
CREATE TABLE IF NOT EXISTS agent_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_messages_workspace ON agent_messages(workspace_id, id);
