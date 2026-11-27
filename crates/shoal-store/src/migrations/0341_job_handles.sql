-- A CI job's handle is its column's alone, which the preparation filled
-- from the job's JSON: a job's queue entry and a claimed job need one, and
-- a repository queues each handle once. The rebuilt queue keeps the last
-- entry's number, so numbers are not reused.
CREATE TEMP TABLE kept_queue_sequence AS SELECT seq FROM sqlite_sequence WHERE name='agent_queue';

CREATE TABLE agent_queue_handled (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('launch','restore','job')),
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    plan TEXT,
    env TEXT,
    execution_id TEXT UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('queued','starting')),
    created_at INTEGER NOT NULL,
    seen_at INTEGER,
    space_id TEXT,
    opens_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    base_branch TEXT,
    automatic INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1)),
    job TEXT,
    handle TEXT,
    mode TEXT,
    narrowing TEXT,
    scope_space_id TEXT,
    priority INTEGER CHECK(priority BETWEEN 0 AND 100),
    stop_reason TEXT,
    wakes_when TEXT,
    issue_labels TEXT,
    source TEXT,
    starts_at_once INTEGER NOT NULL DEFAULT 0,
    CHECK(CASE kind
        WHEN 'launch' THEN plan IS NOT NULL AND env IS NOT NULL AND execution_id IS NULL
            AND job IS NULL AND handle IS NULL
        WHEN 'restore' THEN plan IS NULL AND execution_id IS NOT NULL AND space_id IS NOT NULL
            AND job IS NULL AND handle IS NULL
        ELSE plan IS NULL AND env IS NULL AND execution_id IS NULL AND job IS NOT NULL
            AND handle IS NOT NULL
    END)
);
INSERT INTO agent_queue_handled(seq,id,kind,repository_id,name,agent,issue_title,plan,env,
        execution_id,state,created_at,seen_at,space_id,opens_workspace_id,base_branch,automatic,
        job,handle,mode,narrowing,scope_space_id,priority,stop_reason,wakes_when,issue_labels,
        source,starts_at_once)
    SELECT seq,id,kind,repository_id,name,agent,issue_title,plan,env,execution_id,state,
        created_at,seen_at,space_id,opens_workspace_id,base_branch,automatic,job,handle,mode,
        narrowing,scope_space_id,priority,stop_reason,wakes_when,issue_labels,source,
        starts_at_once
    FROM agent_queue ORDER BY seq;
-- Other tables' triggers name the queue, which the rename leaves to name
-- the rebuilt one.
PRAGMA legacy_alter_table=ON;
DROP TABLE agent_queue;
ALTER TABLE agent_queue_handled RENAME TO agent_queue;
PRAGMA legacy_alter_table=OFF;
DELETE FROM sqlite_sequence WHERE name='agent_queue';
INSERT INTO sqlite_sequence(name,seq) SELECT 'agent_queue',seq FROM kept_queue_sequence;
DROP TABLE kept_queue_sequence;
CREATE INDEX agent_queue_opens ON agent_queue(opens_workspace_id);
CREATE UNIQUE INDEX agent_queue_job_handles ON agent_queue(repository_id, handle)
    WHERE kind = 'job';
CREATE INDEX agent_queue_scopes ON agent_queue(scope_space_id);
CREATE INDEX agent_queue_space ON agent_queue(space_id);
CREATE TRIGGER space_turn_retry_restore_ended AFTER DELETE ON agent_queue
WHEN OLD.execution_id IS NOT NULL BEGIN
    DELETE FROM space_turn_retries WHERE execution_id=OLD.execution_id AND delivered=0
        AND NOT EXISTS (SELECT 1 FROM executions WHERE id=OLD.execution_id);
END;

CREATE TABLE ci_jobs_handled (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL UNIQUE REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT,
    job TEXT NOT NULL,
    handle TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    launched_at INTEGER,
    peak_memory INTEGER,
    usage_recorded INTEGER NOT NULL DEFAULT 0 CHECK(usage_recorded IN (0,1)),
    runner_id INTEGER,
    cut_short INTEGER NOT NULL DEFAULT 0 CHECK(cut_short IN (0,1)),
    oom_killed INTEGER NOT NULL DEFAULT 0 CHECK(oom_killed IN (0,1))
);
INSERT INTO ci_jobs_handled(id,repository_id,workspace_id,execution_id,job,handle,started_at,
        launched_at,peak_memory,usage_recorded,runner_id,cut_short,oom_killed)
    SELECT id,repository_id,workspace_id,execution_id,job,handle,started_at,launched_at,
        peak_memory,usage_recorded,runner_id,cut_short,oom_killed
    FROM ci_jobs;
DROP TABLE ci_jobs;
ALTER TABLE ci_jobs_handled RENAME TO ci_jobs;
