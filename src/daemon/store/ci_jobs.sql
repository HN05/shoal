-- The CI jobs the runner claimed. Each runs in its own workspace at the
-- job's commit, by the execution that runs the forge's runner once it
-- started, and leaves with that workspace once the execution ends. Its peaks
-- are the most CPU, in percent of one core, and resident memory its
-- workspace's usage samples showed, which join the job's usage once, when
-- it ends.
CREATE TABLE IF NOT EXISTS ci_jobs (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL UNIQUE REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT,
    job TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    peak_cpu REAL,
    peak_memory INTEGER,
    usage_recorded INTEGER NOT NULL DEFAULT 0 CHECK(usage_recorded IN (0,1))
);

-- The peaks of each repository's recent runs of a job, by its workflow
-- and name, which a new run of it is expected to reach.
CREATE TABLE IF NOT EXISTS ci_job_usage (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workflow TEXT NOT NULL,
    name TEXT NOT NULL,
    ended_at INTEGER NOT NULL,
    peak_cpu REAL NOT NULL,
    peak_memory INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ci_job_usage_jobs ON ci_job_usage(repository_id, workflow, name, ended_at);
