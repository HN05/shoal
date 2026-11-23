-- What tests that emulate an earlier database run first to undo
-- ci_job_rerun_causes.sql.
ALTER TABLE ci_job_reruns DROP COLUMN cut_off_by;
