-- Local merges into a repository's default branch, so shoal unmerge can undo
-- one while its result is unpushed. A record outlives its workspace.
CREATE TABLE merges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    default_branch TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    workspace TEXT NOT NULL,
    branch TEXT NOT NULL,
    source_commit TEXT NOT NULL,
    previous_commit TEXT NOT NULL,
    result_commit TEXT NOT NULL,
    merged_at INTEGER NOT NULL
);
CREATE INDEX merges_by_branch ON merges(repository_id, branch);
