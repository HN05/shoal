-- A CI job's handle moves out of the job's JSON into a column of its own,
-- which the next migration's preparation fills.
ALTER TABLE agent_queue ADD COLUMN handle TEXT;
ALTER TABLE ci_jobs ADD COLUMN handle TEXT;
