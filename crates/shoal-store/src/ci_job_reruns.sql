-- A CI job protection stopped, which Shoal reruns on its forge once
-- readings recover: the job `job_name`, of ID `job_id` among jobs of that
-- name, of the run at `run_url`.
CREATE TABLE ci_job_reruns (
    run_url TEXT NOT NULL,
    job_id INTEGER NOT NULL,
    job_name TEXT NOT NULL,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    stopped_at INTEGER NOT NULL,
    PRIMARY KEY(run_url, job_id)
);

