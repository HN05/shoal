-- The runs table also keeps seed builds' runs, whose key is the JSON string
-- "seed", and a command's key is the JSON object naming its configured name
-- or its exact arguments.
CREATE TABLE run_usage_kinds (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('ci_job','command','seed_build')),
    key TEXT NOT NULL,
    ended_at INTEGER NOT NULL,
    peak_memory INTEGER NOT NULL,
    duration INTEGER,
    whole INTEGER NOT NULL CHECK(whole IN (0,1)),
    oom_killed INTEGER NOT NULL CHECK(oom_killed IN (0,1))
);
INSERT INTO run_usage_kinds SELECT * FROM run_usage ORDER BY rowid;
DROP TABLE run_usage;
ALTER TABLE run_usage_kinds RENAME TO run_usage;
CREATE INDEX run_usage_runs ON run_usage(repository_id, kind, key, ended_at);
