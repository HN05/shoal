-- The identity of the worktree directory of a disposable workspace whose
-- ownership a removal verified before Git began deleting it, so that a
-- later removal tells what Git left there from a directory that replaced
-- it once Git deleted the `.git` link and admin directory that proved it.
CREATE TABLE removal_directories (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    identity TEXT NOT NULL
);
