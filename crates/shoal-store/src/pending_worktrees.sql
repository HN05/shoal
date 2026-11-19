-- How to create the worktree of a recorded workspace that has none yet, as
-- JSON: its preparation creates it first, and a retried preparation again
-- after a failure. The row goes once the worktree exists.
CREATE TABLE pending_worktrees (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    plan TEXT NOT NULL
);
