-- What cut each CI job waiting to rerun off, as the user's notice of its
-- rerun names it: the machine's sleep or a shutdown. None for a job load
-- protection stopped, whose rerun Shoal makes without a notice.
ALTER TABLE ci_job_reruns ADD COLUMN cut_off_by TEXT;
