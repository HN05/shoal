-- What tests that emulate an earlier database run first to undo
-- ci_job_durations_dropped.sql.
ALTER TABLE ci_job_usage ADD COLUMN seconds INTEGER;
