-- The CI jobs the runner claimed. Each runs in its own workspace at the
-- job's commit, by the execution that runs the forge's runner once it
-- started, and leaves with that workspace once the execution ends.
CREATE TABLE IF NOT EXISTS ci_jobs (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL UNIQUE REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT,
    job TEXT NOT NULL,
    started_at INTEGER NOT NULL
);
