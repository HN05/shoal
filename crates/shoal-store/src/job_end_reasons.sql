-- Every job Shoal ended so far ended once its runner outlived its run.
UPDATE space_events
    SET record=json_set(record,'$.job.reason','its runner outlived its finished run')
    WHERE json_extract(record,'$.kind')='job_ended';
