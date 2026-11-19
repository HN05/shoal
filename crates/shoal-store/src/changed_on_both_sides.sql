-- The sensitive paths that a workspace's HEAD and its default conflict target
-- both changed, as the last check found them; merging that HEAD asks first.
CREATE TABLE changed_on_both_sides (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    target TEXT NOT NULL,
    target_commit TEXT NOT NULL,
    head TEXT NOT NULL,
    files TEXT NOT NULL
);
