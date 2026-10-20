-- Issues and PRs a workspace records beside its assigned ones: those it
-- opened through Shoal and those linked as related. They have no lifecycle
-- effect and go with the workspace.
CREATE TABLE workspace_items (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    relation TEXT NOT NULL CHECK(relation IN ('opened','related')),
    kind TEXT NOT NULL CHECK(kind IN ('issue','pr')),
    url TEXT NOT NULL,
    title TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(workspace_id,relation,url)
);
