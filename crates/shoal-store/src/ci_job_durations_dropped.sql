-- Nothing reads how long a CI job's runs took since idle stops no longer
-- weigh how long the CI an agent waits for has left.
ALTER TABLE ci_job_usage DROP COLUMN seconds;
