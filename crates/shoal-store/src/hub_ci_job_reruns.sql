-- The CI jobs a sleeping or shutting-down machine cut off and handed the
-- hub to rerun once their runs finish, in place of that machine: the job
-- `job_id`, named `job_name`, of the run at `run_url`, stopped at
-- `stopped_at` by the hub's clock by what `cut_off_by` names.
CREATE TABLE hub_ci_job_reruns (
    run_url TEXT NOT NULL,
    job_id INTEGER NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    job_name TEXT NOT NULL,
    cut_off_by TEXT NOT NULL,
    stopped_at INTEGER NOT NULL,
    PRIMARY KEY(run_url, job_id)
);
