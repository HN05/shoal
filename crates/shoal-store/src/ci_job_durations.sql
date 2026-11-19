-- How long each recent run of a CI job took from its start, so Shoal can
-- tell how long a run in progress has left; unknown for runs before.
ALTER TABLE ci_job_usage ADD COLUMN seconds INTEGER;
