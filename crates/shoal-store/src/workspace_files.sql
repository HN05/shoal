-- The files attached to workspaces, by Shoal's copy, and how each came:
-- with a message to the workspace's agents, added on its own, or handed back
-- with `shoal done`. `workspace` is the workspace's name then and
-- `parent_id` its parent's identity, which outlive the workspace.
CREATE TABLE IF NOT EXISTS workspace_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL,
    workspace TEXT NOT NULL,
    parent_id TEXT,
    repository_id TEXT NOT NULL,
    source TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS workspace_files_workspace ON workspace_files(workspace_id, id);
