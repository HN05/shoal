-- Workspaces also link CI runs, as opened or related, so watches follow them.
CREATE TABLE workspace_items_runs (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    relation TEXT NOT NULL CHECK(relation IN ('opened','related')),
    kind TEXT NOT NULL CHECK(kind IN ('issue','pr','ci')),
    url TEXT NOT NULL,
    title TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(workspace_id,relation,url)
);
INSERT INTO workspace_items_runs(workspace_id,relation,kind,url,title,created_at)
    SELECT workspace_id,relation,kind,url,title,created_at FROM workspace_items
    ORDER BY created_at,rowid;
DROP TABLE workspace_items;
ALTER TABLE workspace_items_runs RENAME TO workspace_items;
