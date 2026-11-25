-- What each run Shoal measures used, by repository, kind and key, for the
-- memory later runs of its key are expected to use: when it ended, the most
-- memory it held, how long it ran in seconds, whether it ran whole or was
-- cut short, and whether the kernel's OOM killer killed a process of it. A
-- CI job's key is the JSON array of its workflow and job name. The CI jobs'
-- runs kept before it move here as whole runs of unknown duration.
CREATE TABLE run_usage (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('ci_job','command')),
    key TEXT NOT NULL,
    ended_at INTEGER NOT NULL,
    peak_memory INTEGER NOT NULL,
    duration INTEGER,
    whole INTEGER NOT NULL CHECK(whole IN (0,1)),
    oom_killed INTEGER NOT NULL CHECK(oom_killed IN (0,1))
);
CREATE INDEX run_usage_runs ON run_usage(repository_id, kind, key, ended_at);

INSERT INTO run_usage(repository_id,kind,key,ended_at,peak_memory,duration,whole,oom_killed)
    SELECT repository_id, 'ci_job', json_array(workflow,name), ended_at, peak_memory,
        NULL, 1, 0
    FROM ci_job_usage ORDER BY ended_at, rowid;

DROP TABLE ci_job_usage;

-- Whether something cut a claimed job's run short, as load protection or
-- the user stopping it, and whether the kernel's OOM killer killed a
-- process of it, which its run keeps.
ALTER TABLE ci_jobs ADD COLUMN cut_short INTEGER NOT NULL DEFAULT 0
    CHECK(cut_short IN (0,1));
ALTER TABLE ci_jobs ADD COLUMN oom_killed INTEGER NOT NULL DEFAULT 0
    CHECK(oom_killed IN (0,1));
