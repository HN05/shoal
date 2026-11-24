-- A running review's reopen of a finding resolved `fixed` waits for the
-- review's submit with the `reason` its post gives, which goes once the
-- reopen is posted.
ALTER TABLE review_reopens ADD COLUMN reason TEXT;
