-- CI jobs wait in the agent queue in one order with agent starts. A job's
-- entry carries the waiting job, as JSON, instead of a launch plan or an
-- execution, and leaves the queue once the daemon claims the job. Dropping
-- the old table clears the launches' dependencies, so they are kept aside
-- and restored.
CREATE TEMP TABLE kept_queue_dependencies AS SELECT * FROM agent_queue_dependencies;

CREATE TABLE agent_queue_jobs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('launch','restore','job')),
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    plan TEXT,
    env TEXT,
    execution_id TEXT UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('queued','starting')),
    created_at INTEGER NOT NULL,
    seen_at INTEGER,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    opens_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    base_branch TEXT,
    automatic INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1)),
    job TEXT,
    CHECK(CASE kind
        WHEN 'launch' THEN plan IS NOT NULL AND env IS NOT NULL AND execution_id IS NULL AND job IS NULL
        WHEN 'restore' THEN plan IS NULL AND execution_id IS NOT NULL AND workspace_id IS NOT NULL AND job IS NULL
        ELSE plan IS NULL AND env IS NULL AND execution_id IS NULL AND job IS NOT NULL
    END)
);
INSERT INTO agent_queue_jobs(seq,id,kind,repository_id,name,agent,issue_title,plan,env,
        execution_id,state,created_at,seen_at,workspace_id,opens_workspace_id,base_branch,automatic)
    SELECT seq,id,kind,repository_id,name,agent,issue_title,plan,env,
        execution_id,state,created_at,seen_at,workspace_id,opens_workspace_id,base_branch,automatic
    FROM agent_queue;
DROP TABLE agent_queue;
ALTER TABLE agent_queue_jobs RENAME TO agent_queue;
CREATE INDEX agent_queue_workspace ON agent_queue(workspace_id);
CREATE INDEX agent_queue_opens ON agent_queue(opens_workspace_id);
CREATE UNIQUE INDEX agent_queue_job_handles ON agent_queue(repository_id, json_extract(job, '$.job.handle'))
    WHERE kind = 'job';

INSERT INTO agent_queue_dependencies SELECT * FROM kept_queue_dependencies;
DROP TABLE kept_queue_dependencies;

-- The waiting jobs the runner passed over, cancelled from the queue or
-- superseded by a newer PR head, which it leaves to other runners until the
-- forge no longer lists them.
CREATE TABLE IF NOT EXISTS ci_passed_jobs (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    handle TEXT NOT NULL,
    PRIMARY KEY(repository_id, handle)
);
